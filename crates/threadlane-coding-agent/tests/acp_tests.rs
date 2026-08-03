//! End-to-end ACP client tests.
//!
//! Each test pairs [`AcpConnection`] with an in-process stub agent over a
//! duplex pipe, so the full JSON-RPC framing, request correlation, and
//! bidirectional dispatch are exercised without spawning a real agent binary.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use threadlane_coding_agent::{
    AcpClientHandler, AcpConnection, AcpContentBlock, AcpPermissionOutcome, AcpPermissionPolicy,
    AcpPermissionRequest, AcpProbeClient, AcpReadTextFileRequest, AcpSessionNotification,
    AcpSessionUpdate, AcpStopReason, AcpWorkspaceClient, AcpWriteTextFileRequest,
};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream, ReadHalf};
use tokio::sync::mpsc;

/// Reader/writer pair for the stub agent side of the pipe.
struct StubAgent {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
}

impl StubAgent {
    async fn next_message(&mut self) -> Value {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .expect("stub agent timed out waiting for a client message")
            .expect("stub agent failed to read from the client");
        assert!(read > 0, "client closed the connection unexpectedly");
        serde_json::from_str(line.trim()).expect("client sent invalid JSON")
    }

    async fn send(&mut self, message: Value) {
        let mut encoded = serde_json::to_string(&message).unwrap();
        encoded.push('\n');
        self.writer.write_all(encoded.as_bytes()).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn respond(&mut self, id: &Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await;
    }

    async fn respond_error(&mut self, id: &Value, code: i64, message: &str) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }))
        .await;
    }

    async fn notify_update(&mut self, session_id: &str, update: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": session_id, "update": update },
        }))
        .await;
    }
}

/// Builds a connection whose peer is an in-process stub agent.
fn connect(
    workspace: PathBuf,
    policy: AcpPermissionPolicy,
) -> (
    AcpConnection,
    StubAgent,
    mpsc::UnboundedReceiver<AcpSessionNotification>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handler = Arc::new(
        AcpWorkspaceClient::new(workspace)
            .with_permission_policy(policy)
            .with_update_sender(tx),
    );
    let (connection, stub) = connect_with_handler(handler);
    (connection, stub, rx)
}

/// Builds a connection with an arbitrary handler.
fn connect_with_handler(handler: Arc<dyn AcpClientHandler>) -> (AcpConnection, StubAgent) {
    let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (agent_read, agent_write) = tokio::io::split(agent_io);

    let connection = AcpConnection::from_streams(client_write, client_read, handler, None);
    let stub = StubAgent {
        reader: BufReader::new(agent_read),
        writer: Box::new(agent_write),
    };
    (connection, stub)
}

#[tokio::test]
async fn initialize_negotiates_capabilities_and_agent_info() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    let agent = tokio::spawn(async move {
        let request = stub.next_message().await;
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["protocolVersion"], 1);
        assert_eq!(
            request["params"]["clientCapabilities"]["fs"]["readTextFile"],
            true
        );
        assert_eq!(
            request["params"]["clientCapabilities"]["fs"]["writeTextFile"],
            true
        );
        assert_eq!(request["params"]["clientInfo"]["name"], "threadlane");

        stub.respond(
            &request["id"],
            json!({
                "protocolVersion": 1,
                "agentInfo": { "name": "Stub Agent", "version": "9.9.9" },
                "agentCapabilities": { "loadSession": true },
                "authMethods": [{ "id": "oauth", "name": "Sign in" }],
            }),
        )
        .await;
        stub
    });

    let result = connection.initialize().await.unwrap();
    agent.await.unwrap();

    assert_eq!(result.protocol_version, 1);
    assert_eq!(result.agent_display_name(), "Stub Agent");
    assert!(result.agent_capabilities.load_session);
    assert!(result.requires_authentication());
    assert_eq!(result.auth_methods[0].id, "oauth");
}

#[tokio::test]
async fn initialize_rejects_a_newer_protocol_version() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    tokio::spawn(async move {
        let request = stub.next_message().await;
        stub.respond(&request["id"], json!({ "protocolVersion": 99 }))
            .await;
    });

    let error = connection.initialize().await.unwrap_err();
    assert!(
        error.contains("protocol version 99"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn agent_errors_surface_to_the_caller() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    tokio::spawn(async move {
        let request = stub.next_message().await;
        stub.respond_error(&request["id"], -32000, "agent is not installed")
            .await;
    });

    let error = connection.initialize().await.unwrap_err();
    assert!(error.contains("agent is not installed"), "got: {error}");
    assert!(error.contains("-32000"), "got: {error}");
}

#[tokio::test]
async fn prompt_turn_streams_updates_and_serves_agent_requests() {
    let workspace = tempdir().unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "line1\nline2\nline3\n").unwrap();

    let (connection, mut stub, mut updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::AllowOnce,
    );
    let read_path = workspace.path().join("notes.txt");

    let agent = tokio::spawn(async move {
        // Handshake.
        let init = stub.next_message().await;
        stub.respond(&init["id"], json!({ "protocolVersion": 1 }))
            .await;

        let new_session = stub.next_message().await;
        assert_eq!(new_session["method"], "session/new");
        assert!(new_session["params"]["cwd"].is_string());
        assert_eq!(new_session["params"]["mcpServers"], json!([]));
        stub.respond(&new_session["id"], json!({ "sessionId": "sess_1" }))
            .await;

        // Prompt turn.
        let prompt = stub.next_message().await;
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["sessionId"], "sess_1");
        assert_eq!(
            prompt["params"]["prompt"],
            json!([{ "type": "text", "text": "summarize notes.txt" }])
        );

        stub.notify_update(
            "sess_1",
            json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": { "type": "text", "text": "checking the file" },
            }),
        )
        .await;

        // Agent -> client filesystem request.
        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 900,
            "method": "fs/read_text_file",
            "params": {
                "sessionId": "sess_1",
                "path": read_path.to_string_lossy(),
                "line": 2,
                "limit": 1,
            },
        }))
        .await;
        let read_response = stub.next_message().await;
        assert_eq!(read_response["id"], 900);
        let file_slice = read_response["result"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        // Agent -> client permission request.
        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 901,
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess_1",
                "toolCall": { "toolCallId": "call_1", "title": "Write notes.txt" },
                "options": [
                    { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" },
                ],
            },
        }))
        .await;
        let permission_response = stub.next_message().await;
        assert_eq!(permission_response["id"], 901);
        let outcome = permission_response["result"]["outcome"].clone();

        stub.notify_update(
            "sess_1",
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": file_slice },
            }),
        )
        .await;
        stub.respond(&prompt["id"], json!({ "stopReason": "end_turn" }))
            .await;
        (stub, outcome)
    });

    connection.initialize().await.unwrap();
    let session = connection
        .new_session(workspace.path(), Vec::new())
        .await
        .unwrap();
    assert_eq!(session.session_id, "sess_1");

    let stop_reason = connection
        .prompt(
            &session.session_id,
            vec![AcpContentBlock::text("summarize notes.txt")],
        )
        .await
        .unwrap();
    assert_eq!(stop_reason, AcpStopReason::EndTurn);

    let (_stub, outcome) = agent.await.unwrap();
    assert_eq!(
        outcome,
        json!({ "outcome": "selected", "optionId": "yes" }),
        "AllowOnce policy should select the allow_once option"
    );

    let first = updates.recv().await.unwrap();
    assert_eq!(first.session_id, "sess_1");
    assert_eq!(
        first.update,
        AcpSessionUpdate::AgentThoughtChunk(AcpContentBlock::text("checking the file"))
    );

    let second = updates.recv().await.unwrap();
    // `line: 2, limit: 1` must slice the file, not return the whole thing.
    assert_eq!(
        second.update,
        AcpSessionUpdate::AgentMessageChunk(AcpContentBlock::text("line2"))
    );
}

#[tokio::test]
async fn filesystem_requests_cannot_escape_the_workspace() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "classified").unwrap();

    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::AllowOnce,
    );

    let agent = tokio::spawn(async move {
        let init = stub.next_message().await;
        stub.respond(&init["id"], json!({ "protocolVersion": 1 }))
            .await;

        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 700,
            "method": "fs/read_text_file",
            "params": { "sessionId": "sess_1", "path": secret.to_string_lossy() },
        }))
        .await;
        let read = stub.next_message().await;

        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 701,
            "method": "fs/write_text_file",
            "params": {
                "sessionId": "sess_1",
                "path": "../escaped.txt",
                "content": "nope",
            },
        }))
        .await;
        let write = stub.next_message().await;

        (read, write)
    });

    connection.initialize().await.unwrap();
    let (read, write) = agent.await.unwrap();

    assert!(
        read["result"].is_null(),
        "read outside the workspace must fail"
    );
    assert!(read["error"]["message"]
        .as_str()
        .unwrap()
        .contains("escapes workspace root"));

    assert!(
        write["result"].is_null(),
        "write outside the workspace must fail"
    );
    assert!(write["error"]["message"]
        .as_str()
        .unwrap()
        .contains("escapes workspace root"));
    assert!(!outside.path().join("escaped.txt").exists());
}

#[tokio::test]
async fn rejecting_policy_declines_permission_requests() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    let agent = tokio::spawn(async move {
        let init = stub.next_message().await;
        stub.respond(&init["id"], json!({ "protocolVersion": 1 }))
            .await;
        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 800,
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess_1",
                "toolCall": { "toolCallId": "call_1" },
                "options": [
                    { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" },
                ],
            },
        }))
        .await;
        stub.next_message().await
    });

    connection.initialize().await.unwrap();
    let response = agent.await.unwrap();
    assert_eq!(
        response["result"]["outcome"],
        json!({ "outcome": "selected", "optionId": "no" })
    );
}

#[tokio::test]
async fn unsupported_agent_methods_return_method_not_found() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    let agent = tokio::spawn(async move {
        let init = stub.next_message().await;
        stub.respond(&init["id"], json!({ "protocolVersion": 1 }))
            .await;
        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 600,
            "method": "terminal/create",
            "params": { "sessionId": "sess_1" },
        }))
        .await;
        stub.next_message().await
    });

    connection.initialize().await.unwrap();
    let response = agent.await.unwrap();
    assert_eq!(response["error"]["code"], -32601);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("terminal/create"));
}

#[tokio::test]
async fn cancel_is_sent_as_a_notification() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    let agent = tokio::spawn(async move { stub.next_message().await });

    connection.cancel("sess_1").await.unwrap();
    let message = agent.await.unwrap();
    assert_eq!(message["method"], "session/cancel");
    assert_eq!(message["params"]["sessionId"], "sess_1");
    assert!(
        message["id"].is_null(),
        "session/cancel must not carry a request id"
    );
}

#[tokio::test]
async fn probing_refuses_filesystem_and_permission_requests() {
    let workspace = tempdir().unwrap();
    let readable = workspace.path().join("in-workspace.txt");
    std::fs::write(&readable, "even this is off limits while probing").unwrap();

    let (connection, mut stub) = connect_with_handler(Arc::new(AcpProbeClient));

    let agent = tokio::spawn(async move {
        let init = stub.next_message().await;
        stub.respond(&init["id"], json!({ "protocolVersion": 1 }))
            .await;

        // A probe has no session, so an agent asking for files during the
        // handshake must be refused even for a path the app could otherwise read.
        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 500,
            "method": "fs/read_text_file",
            "params": { "sessionId": "probe", "path": readable.to_string_lossy() },
        }))
        .await;
        let read = stub.next_message().await;

        stub.send(json!({
            "jsonrpc": "2.0",
            "id": 501,
            "method": "session/request_permission",
            "params": {
                "sessionId": "probe",
                "toolCall": { "toolCallId": "call_1" },
                "options": [{ "optionId": "yes", "name": "Allow", "kind": "allow_once" }],
            },
        }))
        .await;
        let permission = stub.next_message().await;

        (read, permission)
    });

    connection.initialize().await.unwrap();
    let (read, permission) = agent.await.unwrap();

    assert!(read["result"].is_null());
    assert!(read["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not available while probing"));
    assert_eq!(
        permission["result"]["outcome"],
        json!({ "outcome": "cancelled" })
    );
}

/// Records update text in arrival order, yielding first so the recording spans
/// an await point. Any realistic handler has one (a channel send, a UI hop);
/// without it a task-per-notification scheduler can look ordered by accident.
struct OrderRecordingClient {
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AcpClientHandler for OrderRecordingClient {
    async fn on_session_update(&self, notification: AcpSessionNotification) {
        tokio::task::yield_now().await;
        if let AcpSessionUpdate::AgentMessageChunk(block) = notification.update {
            if let Some(text) = block.as_text() {
                self.seen.lock().unwrap().push(text.to_string());
            }
        }
    }

    async fn request_permission(&self, _request: AcpPermissionRequest) -> AcpPermissionOutcome {
        AcpPermissionOutcome::Cancelled
    }

    async fn read_text_file(&self, _request: AcpReadTextFileRequest) -> Result<String, String> {
        Err("unused".to_string())
    }

    async fn write_text_file(&self, _request: AcpWriteTextFileRequest) -> Result<(), String> {
        Err("unused".to_string())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_updates_arrive_in_the_order_the_agent_sent_them() {
    const CHUNKS: usize = 200;

    let handler = Arc::new(OrderRecordingClient {
        seen: Mutex::new(Vec::new()),
    });
    let (connection, mut stub) = connect_with_handler(handler.clone());

    let agent = tokio::spawn(async move {
        let init = stub.next_message().await;
        stub.respond(&init["id"], json!({ "protocolVersion": 1 }))
            .await;
        for index in 0..CHUNKS {
            stub.notify_update(
                "sess_1",
                json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": index.to_string() },
                }),
            )
            .await;
        }
        stub
    });

    connection.initialize().await.unwrap();
    let _stub = agent.await.unwrap();

    // A streamed reply only reconstructs in order. Dispatching notifications on
    // separate tasks lets them interleave once the runtime has more than one
    // worker thread.
    let expected: Vec<String> = (0..CHUNKS).map(|index| index.to_string()).collect();
    for _ in 0..50 {
        if handler.seen.lock().unwrap().len() == CHUNKS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = handler.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), CHUNKS, "not every chunk was delivered");
    assert_eq!(seen, expected, "chunks were delivered out of order");
}

#[tokio::test]
async fn shutdown_fails_an_in_flight_prompt() {
    let workspace = tempdir().unwrap();
    let (connection, mut stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );
    let connection = Arc::new(connection);

    // `session/prompt` has no timeout, so shutting the connection down is the
    // only thing that can release a caller waiting on an unanswered turn.
    let pending = tokio::spawn({
        let connection = Arc::clone(&connection);
        async move {
            connection
                .prompt("sess_1", vec![AcpContentBlock::text("hello")])
                .await
        }
    });
    let sent = stub.next_message().await;
    assert_eq!(sent["method"], "session/prompt");

    connection.shutdown().await;

    let error = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("shutdown must not leave a prompt hanging")
        .unwrap()
        .unwrap_err();
    assert!(
        error.contains("shut down") || error.contains("closed the connection"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn pending_requests_fail_when_the_agent_disconnects() {
    let workspace = tempdir().unwrap();
    let (connection, stub, _updates) = connect(
        workspace.path().to_path_buf(),
        AcpPermissionPolicy::default(),
    );

    tokio::spawn(async move {
        // Drop both halves of the stub without answering.
        drop(stub);
    });

    let error = connection.initialize().await.unwrap_err();
    assert!(
        error.contains("closed the connection"),
        "unexpected error: {error}"
    );
}
