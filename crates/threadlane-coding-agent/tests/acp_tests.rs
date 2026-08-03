//! End-to-end ACP client tests.
//!
//! Each test pairs [`AcpConnection`] with an in-process stub agent over a
//! duplex pipe, so the full JSON-RPC framing, request correlation, and
//! bidirectional dispatch are exercised without spawning a real agent binary.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use threadlane_coding_agent::{
    AcpConnection, AcpContentBlock, AcpPermissionPolicy, AcpSessionNotification, AcpSessionUpdate,
    AcpStopReason, AcpWorkspaceClient,
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
    let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (agent_read, agent_write) = tokio::io::split(agent_io);

    let (tx, rx) = mpsc::unbounded_channel();
    let handler = Arc::new(
        AcpWorkspaceClient::new(workspace)
            .with_permission_policy(policy)
            .with_update_sender(tx),
    );

    let connection = AcpConnection::from_streams(client_write, client_read, handler, None);
    let stub = StubAgent {
        reader: BufReader::new(agent_read),
        writer: Box::new(agent_write),
    };
    (connection, stub, rx)
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
