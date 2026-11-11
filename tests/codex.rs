#[path = "../src/codex.rs"]
#[allow(dead_code)]
// Adapter is not yet wired into the library; integration tests include it directly.
mod codex;

use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

async fn send<W: AsyncWrite + Unpin>(peer: &mut W, message: Value) {
    peer.write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}

async fn recv<R: AsyncBufRead + Unpin>(peer: &mut R) -> Value {
    let mut line = String::new();
    peer.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn handshake_and_completed_command_expose_thread_turn_and_events() {
    let (client, peer) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(client);
    let server = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let initialize = recv(&mut peer).await;
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["clientInfo"]["name"], "repoconvoy");
        send(peer.get_mut(), json!({"id":initialize["id"],"result":{}})).await;
        assert_eq!(recv(&mut peer).await["method"], "initialized");
        let start = recv(&mut peer).await;
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"]["cwd"], "C:/repo/isolated");
        assert_eq!(start["params"]["sandbox"], "workspace-write");
        assert_eq!(start["params"]["approvalPolicy"], "on-request");
        assert!(start["params"].get("ephemeral").is_none());
        send(
            peer.get_mut(),
            json!({"id":start["id"],"result":{"thread":{"id":"thread-7"}}}),
        )
        .await;
        let turn = recv(&mut peer).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "thread-7");
        assert_eq!(
            turn["params"]["input"],
            json!([{"type":"text","text":"Edit code"}])
        );
        send(
            peer.get_mut(),
            json!({"id":turn["id"],"result":{"turn":{"id":"turn-8","status":"inProgress"}}}),
        )
        .await;
        send(peer.get_mut(), json!({"method":"item/started","params":{"threadId":"thread-7","turnId":"turn-8","item":{"type":"commandExecution","id":"cmd-1","command":"cargo test","status":"inProgress"}}})).await;
        send(peer.get_mut(), json!({"method":"item/completed","params":{"threadId":"thread-7","turnId":"turn-8","item":{"type":"commandExecution","id":"cmd-1","command":"cargo test","status":"completed","exitCode":0}}})).await;
        send(peer.get_mut(), json!({"method":"turn/completed","params":{"threadId":"thread-7","turn":{"id":"turn-8","status":"completed"}}})).await;
    });
    let mut events = Vec::new();
    let result = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo/isolated"),
        "Edit code",
        Duration::from_secs(2),
        &mut |event| events.push(event),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(result.thread_id, "thread-7");
    assert_eq!(result.turn_id, "turn-8");
    assert_eq!(result.commands_completed, 1);
    assert!(events.iter().any(|e| e.kind == "commandExecution"
        && e.status.as_deref() == Some("completed")
        && e.turn_id == "turn-8"));
}

#[tokio::test]
async fn early_scoped_notifications_are_replayed_after_rpc_responses() {
    let (client, peer) = tokio::io::duplex(16 * 1024);
    let (read, write) = tokio::io::split(client);
    let server = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let init = recv(&mut peer).await;
        send(peer.get_mut(), json!({"id":init["id"],"result":{}})).await;
        assert_eq!(recv(&mut peer).await["method"], "initialized");
        let start = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"method":"thread/started","params":{"threadId":"t"}}),
        )
        .await;
        send(
            peer.get_mut(),
            json!({"id":start["id"],"result":{"thread":{"id":"t"}}}),
        )
        .await;
        let turn = recv(&mut peer).await;
        send(peer.get_mut(), json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"commandExecution","id":"early","status":"completed","exitCode":0}}})).await;
        send(
            peer.get_mut(),
            json!({"id":turn["id"],"result":{"turn":{"id":"u"}}}),
        )
        .await;
        send(peer.get_mut(), json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"completed"}}})).await;
    });
    let mut events = Vec::new();
    let result = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo/isolated"),
        "Edit",
        Duration::from_secs(2),
        &mut |event| events.push(event),
    )
    .await
    .unwrap();
    assert_eq!(result.commands_completed, 1);
    assert!(events.iter().any(|e| e.item_id.as_deref() == Some("early")));
    server.await.unwrap();
}

#[tokio::test]
async fn unscoped_provider_error_before_response_is_not_discarded() {
    let (client, peer) = tokio::io::duplex(16 * 1024);
    let (read, write) = tokio::io::split(client);
    let server = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let init = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"method":"error","params":{"error":"provider unavailable"}}),
        )
        .await;
        send(peer.get_mut(), json!({"id":init["id"],"result":{}})).await;
        send(
            peer.get_mut(),
            json!({"id":2,"result":{"thread":{"id":"t"}}}),
        )
        .await;
        send(peer.get_mut(), json!({"id":3,"result":{"turn":{"id":"u"}}})).await;
        send(peer.get_mut(), json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"fileChange","id":"p","status":"completed"}}})).await;
        send(peer.get_mut(), json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"completed"}}})).await;
        let mut line = String::new();
        while peer.read_line(&mut line).await.unwrap() != 0 {
            line.clear();
        }
    });
    let result = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo/isolated"),
        "Edit",
        Duration::from_secs(2),
        &mut |_| {},
    )
    .await;
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("provider unavailable"),
        "{error}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn worker_launch_strips_git_repository_routing_environment() {
    let mut cmd = if cfg!(windows) {
        let mut command = tokio::process::Command::new("cmd.exe");
        command.args(["/C", "set", "GIT_"]);
        command
    } else {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "env | grep '^GIT_' || true"]);
        command
    };
    cmd.env("GIT_DIR", "Z:/definitely-not-repo")
        .env("GIT_WORK_TREE", "Z:/definitely-not-worktree")
        .env("GIT_CONFIG_COUNT", "1");
    codex::strip_git_environment(&mut cmd);
    let output = cmd.output().await.unwrap();
    assert!(
        output.stdout.is_empty(),
        "Git routing variables leaked to Codex child"
    );
}

#[tokio::test]
async fn app_server_process_is_terminated_and_reaped_before_handoff() {
    let mut command = if cfg!(windows) {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
        cmd
    } else {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30");
        cmd
    };
    let mut child = command.kill_on_drop(true).spawn().unwrap();
    let exit = codex::terminate_and_wait(&mut child).await.unwrap();
    assert!(
        !exit.success(),
        "the long-running server must be stopped, not left alive"
    );
    assert!(
        child.try_wait().unwrap().is_some(),
        "the process must be reaped"
    );
}

async fn exercise(
    frames: Vec<Value>,
) -> (
    std::io::Result<codex::WorkerResult>,
    Vec<codex::WorkerEvent>,
) {
    let (client, peer) = tokio::io::duplex(256 * 1024);
    let (read, write) = tokio::io::split(client);
    let server = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let init = recv(&mut peer).await;
        send(peer.get_mut(), json!({"id":init["id"],"result":{}})).await;
        assert_eq!(recv(&mut peer).await["method"], "initialized");
        let start = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"id":start["id"],"result":{"thread":{"id":"t"}}}),
        )
        .await;
        let turn = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"id":turn["id"],"result":{"turn":{"id":"u","status":"inProgress"}}}),
        )
        .await;
        for frame in frames {
            send(peer.get_mut(), frame).await;
        }
    });
    let mut events = Vec::new();
    let result = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo"),
        "Edit code",
        Duration::from_millis(500),
        &mut |e| events.push(e),
    )
    .await;
    server.await.unwrap();
    (result, events)
}

fn completed() -> Value {
    json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"completed"}}})
}

#[tokio::test]
async fn tool_failure_is_not_overridden_by_completed_turn_or_zero_process_exit() {
    let (result, events) = exercise(vec![
        json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"commandExecution","id":"bad","status":"failed","exitCode":0}}}),
        completed(),
    ]).await;
    assert!(result.unwrap_err().to_string().contains("failed"));
    assert_eq!(events[0].status.as_deref(), Some("failed"));
}

#[tokio::test]
async fn command_completed_with_nonzero_exit_is_failure() {
    let (result, _) = exercise(vec![
        json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"commandExecution","id":"bad","status":"completed","exitCode":1}}}),
        completed(),
    ]).await;
    assert!(result.unwrap_err().to_string().contains("nonzero"));
}

#[tokio::test]
async fn prose_only_completion_is_not_coding_success() {
    let (result, events) = exercise(vec![completed()]).await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("without a successful command")
    );
    assert_eq!(events[0].kind, "turn");
}

#[tokio::test]
async fn successful_file_change_counts_as_executed_tool() {
    let (result, events) = exercise(vec![
        json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"fileChange","id":"patch","status":"completed","changes":[]}}}),
        completed(),
    ]).await;
    assert_eq!(result.unwrap().file_changes_completed, 1);
    assert_eq!(events[0].item_id.as_deref(), Some("patch"));
}

#[tokio::test]
async fn interrupted_turn_is_terminal_failure() {
    let (result, events) = exercise(vec![json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"interrupted","error":{"message":"stopped"}}}})]).await;
    assert!(result.unwrap_err().to_string().contains("interrupted"));
    assert_eq!(events[0].status.as_deref(), Some("interrupted"));
}

#[tokio::test]
async fn approval_request_fails_closed_without_reply() {
    let (result, _) = exercise(vec![json!({"id":44,"method":"item/commandExecution/requestApproval","params":{"threadId":"t","turnId":"u"}})]).await;
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[tokio::test]
async fn malformed_and_oversized_frames_fail_closed() {
    for bytes in [b"{not-json}\n".to_vec(), vec![b'x'; 65_537]] {
        let (client, peer) = tokio::io::duplex(256 * 1024);
        let (read, write) = tokio::io::split(client);
        let server = tokio::spawn(async move {
            let mut peer = BufReader::new(peer);
            let _ = recv(&mut peer).await;
            peer.get_mut().write_all(&bytes).await.unwrap();
        });
        let result = codex::run_protocol(
            read,
            write,
            Path::new("C:/repo"),
            "Edit code",
            Duration::from_secs(1),
            &mut |_| {},
        )
        .await;
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn rejects_scoped_tool_event_before_turn_response_instead_of_dropping_failure() {
    let (client, peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(client);
    let server = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let init = recv(&mut peer).await;
        send(peer.get_mut(), json!({"id":init["id"],"result":{}})).await;
        recv(&mut peer).await;
        let start = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"id":start["id"],"result":{"thread":{"id":"t"}}}),
        )
        .await;
        let turn = recv(&mut peer).await;
        send(peer.get_mut(), json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"commandExecution","id":"early-fail","status":"failed","exitCode":1}}})).await;
        send(
            peer.get_mut(),
            json!({"id":turn["id"],"result":{"turn":{"id":"u"}}}),
        )
        .await;
    });
    let error = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo"),
        "Edit code",
        Duration::from_secs(1),
        &mut |_| {},
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    server.await.unwrap();
}

#[tokio::test]
async fn rejects_foreign_scoped_notifications_even_when_unknown_method() {
    let (result, _) = exercise(vec![
        json!({"method":"turn/diff/updated","params":{"threadId":"foreign","turnId":"u","diff":"x"}}),
        json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"fileChange","id":"p","status":"completed"}}}),
        completed(),
    ]).await;
    assert!(result.unwrap_err().to_string().contains("foreign thread"));
}

#[tokio::test]
#[ignore = "requires local Codex CLI; no model turn is started"]
async fn real_app_server_no_model_handshake() {
    let executable = std::env::var("CODEX_EXE").expect("set CODEX_EXE to the native codex.exe");
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let mut child = tokio::process::Command::new(executable)
        .arg("app-server")
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let handshake = async {
        send(&mut input, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"repoconvoy","version":"0.1.0"}}})).await;
        loop {
            let message = recv(&mut output).await;
            if message["id"] == 1 {
                assert!(message["result"].is_object(), "{message}");
                break;
            }
        }
        send(&mut input, json!({"method":"initialized","params":{}})).await;
        send(&mut input, json!({"id":2,"method":"thread/start","params":{"cwd":cwd.to_str().unwrap(),"sandbox":"workspace-write","approvalPolicy":"on-request"}})).await;
        let mut service_methods = Vec::new();
        loop {
            let message = recv(&mut output).await;
            if message["id"] == 2 {
                assert!(message["result"]["thread"]["id"].is_string(), "{message}");
                assert_eq!(
                    Path::new(message["result"]["cwd"].as_str().unwrap())
                        .canonicalize()
                        .unwrap(),
                    cwd
                );
                break;
            }
            service_methods.push(message["method"].as_str().unwrap().to_owned());
        }
        service_methods
    };
    let methods = tokio::time::timeout(Duration::from_secs(15), handshake)
        .await
        .unwrap();
    println!("real app-server pre-thread notifications: {methods:?}");
    drop(child);
}

#[tokio::test]
async fn silent_peer_expires_deadline() {
    let (client, _peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(client);
    let result = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo"),
        "Edit code",
        Duration::from_millis(20),
        &mut |_| {},
    )
    .await;
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn tolerates_unscoped_service_notifications_before_thread_response() {
    let (client, peer) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(client);
    let server = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let init = recv(&mut peer).await;
        send(peer.get_mut(), json!({"id":init["id"],"result":{}})).await;
        recv(&mut peer).await;
        let start = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"method":"remoteControl/status/changed","params":{"status":"disconnected"}}),
        )
        .await;
        send(
            peer.get_mut(),
            json!({"method":"account/updated","params":{"authMode":"chatgpt"}}),
        )
        .await;
        send(
            peer.get_mut(),
            json!({"id":start["id"],"result":{"thread":{"id":"t"}}}),
        )
        .await;
        let turn = recv(&mut peer).await;
        send(
            peer.get_mut(),
            json!({"id":turn["id"],"result":{"turn":{"id":"u"}}}),
        )
        .await;
        send(
            peer.get_mut(),
            json!({"method":"config/warning","params":{"message":"service ready"}}),
        )
        .await;
        send(peer.get_mut(), json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"fileChange","id":"p","status":"completed"}}})).await;
        send(peer.get_mut(), completed()).await;
    });
    let result = codex::run_protocol(
        read,
        write,
        Path::new("C:/repo"),
        "Edit code",
        Duration::from_secs(1),
        &mut |_| {},
    )
    .await;
    assert_eq!(result.unwrap().file_changes_completed, 1);
    server.await.unwrap();
}

#[tokio::test]
#[ignore = "opt-in genuine Codex model edit in a disposable, remote-free Git fixture"]
async fn real_app_server_worker_edits_then_controller_commits_disposable_fixture() {
    let cwd = std::path::PathBuf::from(
        std::env::var_os("REPOCONVOY_CODEX_CANARY_CWD").expect("set disposable fixture path"),
    );
    assert!(cwd.join(".git").exists());
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap()
    };
    assert!(
        git(&["remote"]).stdout.is_empty(),
        "fixture must have no remote"
    );
    assert!(git(&["status", "--porcelain"]).stdout.is_empty());
    let before = git(&["rev-parse", "HEAD"]);
    assert!(before.status.success());
    let mut events = Vec::new();
    let result = codex::run_worker(
        &cwd,
        "In this disposable Git checkout only, replace hello.txt with exactly `after native worker` followed by a newline. Read it back using a shell command. Do not run git add/commit/push, access any other repo or path, or make network requests; the controller owns Git commits.",
        Duration::from_secs(150),
        &mut |event| events.push(event),
    )
    .await
    .unwrap();
    assert!(result.commands_completed + result.file_changes_completed > 0);
    assert_eq!(
        std::fs::read_to_string(cwd.join("hello.txt")).unwrap(),
        "after native worker\n"
    );
    assert_eq!(
        String::from_utf8(git(&["status", "--porcelain"]).stdout).unwrap(),
        " M hello.txt\n",
        "the coding worker must change exactly the intended candidate file"
    );
    assert!(git(&["add", "--", "hello.txt"]).status.success());
    assert!(
        git(&[
            "commit",
            "--only",
            "-m",
            "test: controller candidate",
            "--",
            "hello.txt"
        ])
        .status
        .success()
    );
    let after = git(&["rev-parse", "HEAD"]);
    assert!(after.status.success());
    assert_ne!(
        before.stdout, after.stdout,
        "a real candidate commit is required"
    );
    assert!(git(&["status", "--porcelain"]).stdout.is_empty());
    assert!(
        events
            .iter()
            .any(|event| event.kind == "turn" && event.status.as_deref() == Some("completed"))
    );
    if let Some(path) = std::env::var_os("REPOCONVOY_CODEX_CANARY_RESULT") {
        std::fs::write(path, serde_json::json!({"threadId":result.thread_id,"turnId":result.turn_id,"commands":result.commands_completed,"fileChanges":result.file_changes_completed,"before":String::from_utf8_lossy(&before.stdout).trim(),"after":String::from_utf8_lossy(&after.stdout).trim()}).to_string()).unwrap();
    }
}
