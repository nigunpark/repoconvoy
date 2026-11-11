use std::{path::Path, process::Command, sync::Arc};

use axum::{
    Json,
    extract::{Path as AxumPath, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

pub(crate) const PROJECTS: TableDefinition<&str, &str> = TableDefinition::new("projects");
const SESSIONS: TableDefinition<&str, &str> = TableDefinition::new("sessions");

pub(crate) type Result<T> = std::result::Result<T, ApiError>;
pub(crate) struct ApiError(pub StatusCode, pub String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}
pub(crate) fn bad(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}
pub(crate) fn internal(error: impl std::fmt::Display) -> ApiError {
    eprintln!("RepoConvoy storage error: {error}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Storage error".into())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Repository {
    pub name: String,
    pub path: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Project {
    pub id: String,
    pub name: String,
    pub repositories: Vec<Repository>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NewProject {
    name: String,
    repository_paths: Vec<String>,
}

fn validate_repo(path: &str) -> Result<Repository> {
    let canonical = Path::new(path)
        .canonicalize()
        .map_err(|_| bad("Git root does not exist"))?;
    if !canonical.is_dir() {
        return Err(bad("Git root must be a directory"));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|_| bad("Git executable is unavailable"))?;
    if !output.status.success() {
        return Err(bad("Git root is not a Git repository"));
    }
    let top = String::from_utf8(output.stdout).map_err(|_| bad("Git root encoding is invalid"))?;
    let root = Path::new(top.trim())
        .canonicalize()
        .map_err(|_| bad("Git root could not be resolved"))?;
    if root != canonical {
        return Err(bad("Git root must be the top-level folder"));
    }
    Ok(Repository {
        name: canonical
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: canonical
            .to_str()
            .ok_or_else(|| bad("Git root path must be Unicode"))?
            .to_owned(),
    })
}

pub(crate) async fn list_projects(
    State(db): State<Arc<Database>>,
) -> Result<Json<serde_json::Value>> {
    let read = db.begin_read().map_err(internal)?;
    let table = match read.open_table(PROJECTS) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            return Ok(Json(serde_json::json!({"projects": []})));
        }
        Err(error) => return Err(internal(error)),
    };
    let mut projects = Vec::new();
    for entry in table.iter().map_err(internal)? {
        let (_, value) = entry.map_err(internal)?;
        projects.push(serde_json::from_str::<Project>(value.value()).map_err(internal)?);
    }
    Ok(Json(serde_json::json!({"projects": projects})))
}

pub(crate) async fn create_project(
    State(db): State<Arc<Database>>,
    payload: std::result::Result<Json<NewProject>, JsonRejection>,
) -> Result<Json<Project>> {
    let Json(input) =
        payload.map_err(|_| bad("Expected project name and repository paths (invalid JSON)"))?;
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 80 || input.repository_paths.is_empty() {
        return Err(bad("Expected project name and repository paths"));
    }
    let repositories: Vec<_> = input
        .repository_paths
        .iter()
        .map(|path| validate_repo(path))
        .collect::<Result<_>>()?;
    let mut unique = std::collections::HashSet::new();
    if !repositories.iter().all(|r| unique.insert(&r.path)) {
        return Err(bad("Git root duplicated in project"));
    }
    let project = Project {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        repositories,
    };
    let serialized = serde_json::to_string(&project).map_err(internal)?;
    let write = db.begin_write().map_err(internal)?;
    {
        let mut table = write.open_table(PROJECTS).map_err(internal)?;
        for entry in table.iter().map_err(internal)? {
            let (_, old) = entry.map_err(internal)?;
            let existing: Project = serde_json::from_str(old.value()).map_err(internal)?;
            if existing.name.eq_ignore_ascii_case(&project.name)
                || existing
                    .repositories
                    .iter()
                    .any(|r| project.repositories.iter().any(|new| new.path == r.path))
            {
                return Err(bad("Git root or project already registered"));
            }
        }
        table
            .insert(project.id.as_str(), serialized.as_str())
            .map_err(internal)?;
    }
    write.commit().map_err(internal)?;
    Ok(Json(project))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    id: String,
    project_id: String,
    state: String,
    children: Vec<serde_json::Value>,
}
fn not_found(kind: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{kind} not found"))
}
fn project_exists(db: &Database, id: &str) -> Result<bool> {
    let read = db.begin_read().map_err(internal)?;
    let table = match read.open_table(PROJECTS) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(false),
        Err(error) => return Err(internal(error)),
    };
    let exists = table.get(id).map_err(internal)?.is_some();
    Ok(exists)
}

pub(crate) async fn create_session(
    AxumPath(project_id): AxumPath<String>,
    State(db): State<Arc<Database>>,
) -> Result<Json<Session>> {
    if !project_exists(&db, &project_id)? {
        return Err(not_found("Project"));
    }
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        project_id,
        state: "idle".into(),
        children: vec![],
    };
    let serialized = serde_json::to_string(&session).map_err(internal)?;
    let write = db.begin_write().map_err(internal)?;
    {
        let mut table = write.open_table(SESSIONS).map_err(internal)?;
        table
            .insert(session.id.as_str(), serialized.as_str())
            .map_err(internal)?;
    }
    write.commit().map_err(internal)?;
    Ok(Json(session))
}

pub(crate) async fn list_sessions(
    AxumPath(project_id): AxumPath<String>,
    State(db): State<Arc<Database>>,
) -> Result<Json<serde_json::Value>> {
    if !project_exists(&db, &project_id)? {
        return Err(not_found("Project"));
    }
    let read = db.begin_read().map_err(internal)?;
    let table = match read.open_table(SESSIONS) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            return Ok(Json(serde_json::json!({"sessions": []})));
        }
        Err(error) => return Err(internal(error)),
    };
    let mut sessions = Vec::new();
    for entry in table.iter().map_err(internal)? {
        let (_, value) = entry.map_err(internal)?;
        let session: Session = serde_json::from_str(value.value()).map_err(internal)?;
        if session.project_id == project_id {
            sessions.push(session);
        }
    }
    Ok(Json(serde_json::json!({"sessions": sessions})))
}

pub(crate) async fn get_board(
    AxumPath(id): AxumPath<String>,
    State(db): State<Arc<Database>>,
) -> Result<Json<serde_json::Value>> {
    let read = db.begin_read().map_err(internal)?;
    let table = match read.open_table(SESSIONS) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Err(not_found("Session")),
        Err(error) => return Err(internal(error)),
    };
    let row = table
        .get(id.as_str())
        .map_err(internal)?
        .ok_or_else(|| not_found("Session"))?;
    let session: Session = serde_json::from_str(row.value()).map_err(internal)?;
    Ok(Json(
        serde_json::json!({"sessionId": session.id, "projectId": session.project_id,
        "state": session.state, "children": session.children}),
    ))
}
