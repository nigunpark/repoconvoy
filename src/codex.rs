//! Bounded, fail-closed Codex app-server stdio adapter for one coding turn.
//! The controller owns persistence of the returned thread ID; resume is separate.
use std::{
    collections::{HashSet, VecDeque},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

const MAX_LINE: u64 = 64 * 1024;
const MAX_EVENTS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub kind: String,
    pub status: Option<String>,
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResult {
    pub thread_id: String,
    pub turn_id: String,
    pub commands_completed: usize,
    pub file_changes_completed: usize,
}

/// Launch using the user's normal Codex configuration. Never auto-approve escalation.
/// `deadline` bounds the whole handshake and turn, not just inactivity.
pub async fn run_worker(
    cwd: &Path,
    prompt: &str,
    deadline: Duration,
    on_event: &mut impl FnMut(WorkerEvent),
) -> io::Result<WorkerResult> {
    let executable = std::env::var_os("REPOCONVOY_CODEX_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                // npm's `codex` shim is not directly executable by CreateProcess.
                // Prefer its native binary where installed; otherwise use PATH.
                let npm_binary = std::env::var_os("APPDATA").map(|root| {
                    PathBuf::from(root).join("npm/node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe")
                });
                if let Some(binary) = npm_binary.filter(|p| p.is_file()) {
                    return binary;
                }
                PathBuf::from("codex.exe")
            }
            #[cfg(not(windows))]
            {
                PathBuf::from("codex")
            }
        });
    run_worker_with_program(&executable, cwd, prompt, deadline, on_event).await
}

/// Program override is useful for a deterministic fake peer, not for changing Codex policy.
pub async fn run_worker_with_program(
    program: &Path,
    cwd: &Path,
    prompt: &str,
    deadline: Duration,
    on_event: &mut impl FnMut(WorkerEvent),
) -> io::Result<WorkerResult> {
    let cwd = cwd.canonicalize()?;
    let mut command = Command::new(program);
    command
        .arg("app-server")
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    strip_git_environment(&mut command);
    let mut child = command.spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| invalid("missing stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| invalid("missing stdout"))?;
    // Terminate and reap before releasing the repository's single-writer lease.
    let result = run_protocol(stdout, stdin, &cwd, prompt, deadline, on_event).await;
    let reaped = terminate_and_wait(&mut child).await;
    match (result, reaped) {
        (Ok(done), Ok(_)) => Ok(done),
        (Ok(_), Err(error)) | (Err(error), Ok(_)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::new(
            primary.kind(),
            format!("{primary}; Codex cleanup also failed: {cleanup}"),
        )),
    }
}

pub async fn terminate_and_wait(
    child: &mut tokio::process::Child,
) -> io::Result<std::process::ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }
    if let Err(error) = child.start_kill()
        && child.try_wait()?.is_none()
    {
        return Err(error);
    }
    timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Codex process did not terminate"))?
}

/// Git's repository-routing environment can override the isolated child cwd.
pub fn strip_git_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_CONFIG_COUNT"] {
        command.env_remove(key);
    }
}

pub async fn run_protocol<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    read: R,
    write: W,
    cwd: &Path,
    prompt: &str,
    deadline: Duration,
    on_event: &mut impl FnMut(WorkerEvent),
) -> io::Result<WorkerResult> {
    timeout(deadline, protocol(read, write, cwd, prompt, on_event))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Codex turn deadline exceeded"))?
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

async fn send<W: AsyncWrite + Unpin>(write: &mut W, value: Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(&value).map_err(|e| invalid(e.to_string()))?;
    write.write_all(&bytes).await?;
    write.write_all(b"\n").await?;
    write.flush().await
}

async fn receive<R: AsyncRead + Unpin>(read: &mut BufReader<R>) -> io::Result<Value> {
    let mut bytes = Vec::new();
    let count = read
        .take(MAX_LINE + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Codex closed stdout",
        ));
    }
    if count as u64 > MAX_LINE || bytes.last() != Some(&b'\n') {
        return Err(invalid("oversized or unterminated Codex JSONL frame"));
    }
    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
}

fn text<'a>(value: &'a Value, pointer: &str) -> io::Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("missing {pointer}")))
}

async fn response<R: AsyncRead + Unpin>(
    read: &mut BufReader<R>,
    id: u64,
    buffered: &mut VecDeque<Value>,
) -> io::Result<Value> {
    // Initialization can emit asynchronous informational notifications.
    for _ in 0..MAX_EVENTS {
        let value = receive(read).await?;
        if value.get("id").is_some() {
            if value["id"] != id {
                return Err(invalid("unexpected response id"));
            }
            if value.get("error").is_some() {
                return Err(invalid(format!("Codex RPC error: {}", value["error"])));
            }
            if !value["result"].is_object() {
                return Err(invalid("missing RPC result"));
            }
            return Ok(value["result"].clone());
        }
        if !value["method"].is_string() || !value["params"].is_object() {
            return Err(invalid("malformed notification before response"));
        }
        if value["method"] == "error" {
            return Err(invalid(format!(
                "Codex error: {}",
                value["params"]["error"]
            )));
        }
        if value["params"].get("threadId").is_some() || value["params"].get("turnId").is_some() {
            buffered.push_back(value);
            if buffered.len() >= MAX_EVENTS {
                return Err(invalid("too many buffered Codex notifications"));
            }
        }
    }
    Err(invalid("too many notifications before response"))
}

async fn protocol<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    read: R,
    mut write: W,
    cwd: &Path,
    prompt: &str,
    on_event: &mut impl FnMut(WorkerEvent),
) -> io::Result<WorkerResult> {
    let mut read = BufReader::new(read);
    let mut buffered = VecDeque::new();
    send(&mut write, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"repoconvoy","version":env!("CARGO_PKG_VERSION")}}})).await?;
    response(&mut read, 1, &mut buffered).await?;
    send(&mut write, json!({"method":"initialized","params":{}})).await?;
    let cwd = cwd
        .to_str()
        .ok_or_else(|| invalid("non-UTF8 working directory"))?;
    send(&mut write, json!({"id":2,"method":"thread/start","params":{"cwd":cwd,"sandbox":"workspace-write","approvalPolicy":"on-request"}})).await?;
    let started = response(&mut read, 2, &mut buffered).await?;
    let thread_id = text(&started, "/thread/id")?.to_owned();
    send(&mut write, json!({"id":3,"method":"turn/start","params":{"threadId":thread_id,"input":[{"type":"text","text":prompt}]}})).await?;
    let turn = response(&mut read, 3, &mut buffered).await?;
    let turn_id = text(&turn, "/turn/id")?.to_owned();
    let mut result = WorkerResult {
        thread_id,
        turn_id,
        commands_completed: 0,
        file_changes_completed: 0,
    };
    let mut pending = HashSet::new();
    let mut completed = HashSet::new();
    for _ in 0..MAX_EVENTS {
        let value = if let Some(queued) = buffered.pop_front() {
            queued
        } else {
            receive(&mut read).await?
        };
        let method = text(&value, "/method")?;
        // Any server-to-client request is an approval/user-interaction boundary. Do not answer it.
        if value.get("id").is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Codex requested interaction: {method}"),
            ));
        }
        let params = value
            .get("params")
            .filter(|v| v.is_object())
            .ok_or_else(|| invalid("missing notification params"))?;
        if let Some(other) = params.get("threadId")
            && other.as_str() != Some(&result.thread_id)
        {
            return Err(invalid("foreign thread notification"));
        }
        if let Some(other) = params.get("turnId")
            && other.as_str() != Some(&result.turn_id)
        {
            return Err(invalid("foreign turn notification"));
        }
        if method == "error" {
            return Err(invalid(format!("Codex error: {}", params["error"])));
        }
        if !matches!(method, "item/started" | "item/completed" | "turn/completed") {
            continue;
        }
        if text(params, "/threadId")? != result.thread_id {
            return Err(invalid("foreign thread notification"));
        }
        let event_turn = if method == "turn/completed" {
            text(params, "/turn/id")?
        } else {
            text(params, "/turnId")?
        };
        if event_turn != result.turn_id {
            return Err(invalid("foreign turn notification"));
        }
        if method == "turn/completed" {
            let status = text(params, "/turn/status")?;
            on_event(WorkerEvent {
                thread_id: result.thread_id.clone(),
                turn_id: result.turn_id.clone(),
                kind: "turn".into(),
                status: Some(status.into()),
                item_id: None,
            });
            if status != "completed" {
                return Err(invalid(format!(
                    "Codex turn {status}: {}",
                    params["turn"]["error"]
                )));
            }
            if !pending.is_empty() {
                return Err(invalid("Codex turn completed with unfinished tools"));
            }
            if result.commands_completed + result.file_changes_completed == 0 {
                return Err(invalid(
                    "coding task completed without a successful command or file change",
                ));
            }
            return Ok(result);
        }
        let kind = text(params, "/item/type")?;
        if !matches!(kind, "commandExecution" | "fileChange") {
            continue;
        }
        let item_id = text(params, "/item/id")?.to_owned();
        let status = text(params, "/item/status")?;
        if method == "item/started" {
            pending.insert(item_id.clone());
        }
        if method == "item/completed" {
            if !completed.insert(item_id.clone()) {
                return Err(invalid("duplicate completed tool"));
            }
            pending.remove(&item_id);
            if status != "completed"
                || (kind == "commandExecution" && params["item"]["exitCode"].as_i64() != Some(0))
            {
                on_event(WorkerEvent {
                    thread_id: result.thread_id.clone(),
                    turn_id: result.turn_id.clone(),
                    kind: kind.into(),
                    status: Some(status.into()),
                    item_id: Some(item_id),
                });
                return Err(invalid(format!(
                    "Codex {kind} failed or exited nonzero: {}",
                    params["item"]
                )));
            }
            if kind == "commandExecution" {
                result.commands_completed += 1;
            } else {
                result.file_changes_completed += 1;
            }
        }
        on_event(WorkerEvent {
            thread_id: result.thread_id.clone(),
            turn_id: result.turn_id.clone(),
            kind: kind.into(),
            status: Some(status.into()),
            item_id: Some(item_id),
        });
    }
    Err(invalid("Codex event limit exceeded"))
}
