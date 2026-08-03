//! Live end-to-end ACP turn against a real agent.
//!
//! Ignored: needs network, npx, and working Claude credentials. Run with
//! `cargo test -p threadlane-coding-agent --test acp_live_turn -- --ignored --nocapture`.
use std::sync::Arc;
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{
    agent_events_for, AcpAgentConfig, AcpClientHandler, AcpManager, AcpPermissionPolicy, AcpScope,
    AcpSettings, AcpStopReason, AcpWorkspaceClient,
};

#[tokio::test]
#[ignore = "requires network, npx and Claude credentials"]
async fn a_real_acp_turn_streams_mapped_events() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("hello.txt"),
        "hello from threadlane\n",
    )
    .unwrap();

    AcpSettings::save_global(
        dir.path(),
        &[AcpAgentConfig::from_command_line(
            "Claude Code",
            "npx -y @agentclientprotocol/claude-agent-acp",
            AcpScope::Global,
        )
        .unwrap()],
    )
    .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handler: Arc<dyn AcpClientHandler> = Arc::new(
        AcpWorkspaceClient::new(workspace.path().to_path_buf())
            .with_permission_policy(AcpPermissionPolicy::AllowOnce)
            .with_update_sender(tx),
    );

    let manager = AcpManager::new(
        Some(dir.path().to_path_buf()),
        Some(workspace.path().to_path_buf()),
    );
    let session = manager
        .start_session("claude_code", workspace.path(), handler)
        .await
        .expect("session should start");
    println!("agent: {}", session.agent().agent_display_name());

    let prompt = session.prompt_text("Reply with exactly: PONG. Do not use any tools.");
    tokio::pin!(prompt);

    let mut text = String::new();
    let mut event_count = 0usize;
    let stop = loop {
        tokio::select! {
            result = &mut prompt => break result,
            update = rx.recv() => {
                if let Some(n) = update {
                    for event in agent_events_for(n.update) {
                        event_count += 1;
                        if let AgentEvent::MessageUpdate { text_delta: Some(d), .. } = &event {
                            text.push_str(d);
                        }
                    }
                }
            }
        }
    };
    while let Ok(n) = rx.try_recv() {
        for event in agent_events_for(n.update) {
            event_count += 1;
            if let AgentEvent::MessageUpdate {
                text_delta: Some(d),
                ..
            } = &event
            {
                text.push_str(d);
            }
        }
    }

    println!("stop={:?} events={event_count} text={text:?}", stop);
    let stop = match stop {
        Ok(stop) => stop,
        Err(error) if error.contains("authenticate") || error.contains("401") => {
            panic!(
                "the agent could not authenticate, so this machine's Claude \
                 credentials need refreshing rather than the client being at \
                 fault: {error}"
            )
        }
        Err(error) => panic!("prompt failed: {error}"),
    };
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert!(event_count > 0, "expected mapped events");
    assert!(text.to_uppercase().contains("PONG"), "got: {text:?}");
    session.shutdown().await;
}
