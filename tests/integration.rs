use std::{path::PathBuf, process::Command, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("repoconvoy-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn start(state: PathBuf) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        repoconvoy::serve(listener, state).await.unwrap();
    });
    (port, handle)
}
async fn request(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, Vec<u8>) {
    request_with_headers(
        port,
        method,
        path,
        body,
        &format!("Host: localhost:{port}\r\n"),
    )
    .await
}
async fn request_with_headers(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    headers: &str,
) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let body = body.unwrap_or("");
    let wire = format!(
        "{method} {path} HTTP/1.1\r\n{headers}Connection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(wire.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let split = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8_lossy(&bytes[..split]);
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, bytes[split + 4..].to_vec())
}

#[tokio::test]
async fn dns_rebinding_host_cannot_read_registered_absolute_paths() {
    let fixture = Fixture::new();
    let repo = fixture.0.join("private-repo");
    git_repo(&repo);
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let body = serde_json::json!({"name":"Private", "repositoryPaths":[repo.to_str().unwrap()]})
        .to_string();
    let (status, _) = json(port, "POST", "/api/projects", Some(&body)).await;
    assert_eq!(status, 200);
    let (status, bytes) = request_with_headers(
        port,
        "GET",
        "/api/projects",
        None,
        &format!("Host: attacker.example:{port}\r\n"),
    )
    .await;
    assert_eq!(status, 403);
    assert!(!String::from_utf8_lossy(&bytes).contains(repo.to_str().unwrap()));
    handle.abort();
}

#[tokio::test]
async fn rejects_foreign_origin_and_wrong_port_even_on_localhost() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    for headers in [
        format!("Host: localhost:{port}\r\nOrigin: http://attacker.example:{port}\r\n"),
        format!("Host: 127.0.0.1:{port}\r\nOrigin: null\r\n"),
        format!("Host: localhost:{port}\r\nOrigin: http://localhost:9999\r\n"),
        "Host: localhost:9999\r\n".to_string(),
        "Host: 127.0.0.1:9999\r\n".to_string(),
        format!("Host: localhost:{port}.attacker.example\r\n"),
        format!("Host: localhost:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n"),
        format!("Host: localhost:{port}\r\nSec-Fetch-Site: cross-site\r\n"),
    ] {
        let (status, _) = request_with_headers(port, "GET", "/api/projects", None, &headers).await;
        assert_eq!(status, 403, "{headers}");
    }
    for headers in [
        format!("Host: localhost:{port}\r\nOrigin: http://localhost:{port}\r\n"),
        format!("Host: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n"),
    ] {
        let (status, _) = request_with_headers(port, "GET", "/api/projects", None, &headers).await;
        assert_eq!(status, 200, "{headers}");
    }
    handle.abort();
}

#[tokio::test]
async fn api_responses_prevent_caching_and_mime_sniffing() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /api/projects HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let split = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8_lossy(&bytes[..split]).to_ascii_lowercase();
    assert!(head.contains("cache-control: no-store"), "{head}");
    assert!(head.contains("x-content-type-options: nosniff"), "{head}");
    handle.abort();
}

fn git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

async fn json(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, serde_json::Value) {
    let (status, bytes) = request(port, method, path, body).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn original_vector_mascot_replaces_raster_asset() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let (status, icon) = request(port, "GET", "/assets/app-icon.svg", None).await;
    assert_eq!(
        status, 200,
        "new vector icon must be served by the Rust app"
    );
    assert_eq!(icon, include_bytes!("../assets/app-icon.svg"));
    let (status, _) = request(port, "GET", "/assets/app-icon.png", None).await;
    assert_eq!(status, 404, "the old raster icon must not be served");
    for name in ["mascot-original.png", "mascot.png", "app-icon.png"] {
        assert!(
            !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(name)
                .exists(),
            "old image still present: {name}"
        );
    }
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn dashboard_assets_are_served_from_the_real_tree() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    for (route, expected) in [
        ("/", include_bytes!("../web/dashboard.html").as_slice()),
        ("/theme.css", include_bytes!("../web/theme.css").as_slice()),
        ("/theme.js", include_bytes!("../web/theme.js").as_slice()),
        ("/icons.svg", include_bytes!("../web/icons.svg").as_slice()),
        (
            "/assets/app-icon.svg",
            include_bytes!("../assets/app-icon.svg").as_slice(),
        ),
    ] {
        let (status, body) = request(port, "GET", route, None).await;
        assert_eq!(status, 200, "{route}");
        assert_eq!(body, expected, "{route}");
    }
    handle.abort();
}

#[tokio::test]
async fn real_git_root_can_be_registered_and_listed() {
    let fixture = Fixture::new();
    let repo = fixture.0.join("first");
    git_repo(&repo);
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let body = serde_json::json!({"name":"Portfolio", "repositoryPaths":[repo.to_str().unwrap()]})
        .to_string();
    let (status, project) = json(port, "POST", "/api/projects", Some(&body)).await;
    assert_eq!(status, 200, "{project}");
    assert_eq!(project["name"], "Portfolio");
    assert!(project["id"].as_str().is_some());
    assert_eq!(project["repositories"][0]["name"], "first");
    assert_eq!(
        std::fs::canonicalize(project["repositories"][0]["path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(repo).unwrap()
    );
    let (status, list) = json(port, "GET", "/api/projects", None).await;
    assert_eq!(status, 200);
    assert_eq!(list["projects"], serde_json::json!([project]));
    handle.abort();
}

#[tokio::test]
async fn nested_git_folder_is_rejected() {
    let fixture = Fixture::new();
    let repo = fixture.0.join("repo");
    git_repo(&repo);
    let nested = repo.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let body = serde_json::json!({"name":"Nested", "repositoryPaths":[nested.to_str().unwrap()]})
        .to_string();
    let (status, error) = json(port, "POST", "/api/projects", Some(&body)).await;
    assert_eq!(status, 400);
    assert!(error["error"].as_str().unwrap().contains("Git root"));
    let (_, list) = json(port, "GET", "/api/projects", None).await;
    assert_eq!(list["projects"], serde_json::json!([]));
    handle.abort();
}

#[tokio::test]
async fn parent_session_and_board_survive_restart() {
    let fixture = Fixture::new();
    let repo = fixture.0.join("repo");
    git_repo(&repo);
    let state = fixture.0.join("state.redb");
    let (port, handle) = start(state.clone()).await;
    let body = serde_json::json!({"name":"Persistent", "repositoryPaths":[repo.to_str().unwrap()]})
        .to_string();
    let (status, project) = json(port, "POST", "/api/projects", Some(&body)).await;
    assert_eq!(status, 200);
    let path = format!("/api/projects/{}/sessions", project["id"].as_str().unwrap());
    let (status, session) = json(port, "POST", &path, Some("{}")).await;
    assert_eq!(status, 200, "{session}");
    assert_eq!(session["projectId"], project["id"]);
    assert_eq!(session["state"], "idle");
    assert_eq!(session["children"], serde_json::json!([]));
    let (status, list) = json(port, "GET", &path, None).await;
    assert_eq!(status, 200);
    assert_eq!(list["sessions"], serde_json::json!([session]));
    handle.abort();
    let _ = handle.await;
    let (port, handle) = start(state).await;
    let (status, projects) = json(port, "GET", "/api/projects", None).await;
    assert_eq!(status, 200);
    assert_eq!(projects["projects"], serde_json::json!([project]));
    let (status, sessions) = json(port, "GET", &path, None).await;
    assert_eq!(status, 200);
    assert_eq!(sessions["sessions"], serde_json::json!([session]));
    let board_path = format!("/api/sessions/{}/board", session["id"].as_str().unwrap());
    let (status, board) = json(port, "GET", &board_path, None).await;
    assert_eq!(status, 200);
    assert_eq!(
        board,
        serde_json::json!({"sessionId":session["id"], "projectId":project["id"], "state":"idle", "children":[]})
    );
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn dispatch_is_explicitly_unavailable() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let (status, capabilities) = json(port, "GET", "/api/capabilities", None).await;
    assert_eq!(status, 200);
    assert_eq!(capabilities, serde_json::json!({"dispatch":false}));
    let (status, _) = request(
        port,
        "POST",
        "/api/sessions/no-session/orders",
        Some("{\"prompt\":\"test\"}"),
    )
    .await;
    assert_eq!(status, 404);
    handle.abort();
    let _ = handle.await;
}

#[test]
fn cli_exposes_state_and_port_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_repoconvoy"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--state"));
    assert!(help.contains("--port"));
}

#[tokio::test]
async fn repository_cannot_be_registered_by_two_projects() {
    let fixture = Fixture::new();
    let repo = fixture.0.join("shared");
    git_repo(&repo);
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let first =
        serde_json::json!({"name":"First", "repositoryPaths":[repo.to_str().unwrap()]}).to_string();
    let second = serde_json::json!({"name":"Second", "repositoryPaths":[repo.to_str().unwrap()]})
        .to_string();
    let (status, _) = json(port, "POST", "/api/projects", Some(&first)).await;
    assert_eq!(status, 200);
    let (status, error) = json(port, "POST", "/api/projects", Some(&second)).await;
    assert_eq!(status, 400, "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("already registered")
    );
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn dashboard_html_restricts_scripts_and_external_resources() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let head =
        String::from_utf8_lossy(&bytes[..bytes.windows(4).position(|x| x == b"\r\n\r\n").unwrap()]);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-security-policy:"),
        "{head}"
    );
    assert!(head.contains("default-src 'none'"), "{head}");
    assert!(head.contains("img-src 'self'"), "{head}");
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn malformed_project_request_returns_json_error() {
    let fixture = Fixture::new();
    let (port, handle) = start(fixture.0.join("state.redb")).await;
    let (status, error) = json(port, "POST", "/api/projects", Some("{invalid")).await;
    assert_eq!(status, 400);
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("Expected project name")
    );
    handle.abort();
    let _ = handle.await;
}
