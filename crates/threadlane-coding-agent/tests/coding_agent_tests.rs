use std::fs::File;
use std::io::Write;
use tempfile::tempdir;
use threadlane_agent::{AgentConfig, AgentMessage, SessionTree, UnifiedAgent};
use threadlane_coding_agent::{
    execute_slash_command, parse_slash_command, CommandAction, ProjectContext,
};

#[test]
fn test_project_context_discovery() {
    let dir = tempdir().unwrap();
    let agents_file = dir.path().join("AGENTS.md");
    let mut f = File::create(&agents_file).unwrap();
    writeln!(f, "Rule 1: Always write tests.").unwrap();

    let ctx = ProjectContext::discover(dir.path());
    assert_eq!(ctx.context_files.len(), 1);
    assert_eq!(ctx.instructions.len(), 1);
    assert_eq!(ctx.instructions[0].path, agents_file);
    assert_eq!(ctx.instructions[0].content, "Rule 1: Always write tests.");
    assert!(ctx
        .combined_instructions
        .contains("Rule 1: Always write tests."));
}

#[tokio::test]
async fn compact_command_stays_in_current_session() {
    let tmp = tempdir().unwrap();
    let session_file = tmp.path().join("test.jsonl");
    let mut agent = UnifiedAgent::new(
        "fake",
        None,
        "gpt-4o",
        &session_file,
        threadlane_agent::AgentConfig::default(),
    )
    .unwrap();
    let mut tree = SessionTree::new("current_session");
    for index in 0..60 {
        let message = AgentMessage::User {
            content: format!("message {index}"),
        };
        agent.turn.lock().await.messages.push(message.clone());
        tree.add_message(message);
    }

    let output = execute_slash_command(CommandAction::Compact, &mut agent, &mut tree).await;

    assert_eq!(tree.session_id, "current_session");
    assert_eq!(output, "Context compacted in the current session.");
    assert!(tree
        .get_active_branch_messages()
        .iter()
        .any(|message| threadlane_agent::compaction_summary_text(message).is_some()));
}

#[test]
fn test_slash_command_parsing() {
    // Happy paths for existing commands
    assert_eq!(
        parse_slash_command("/model gpt-4o"),
        Some(CommandAction::SwitchModel("gpt-4o".to_string()))
    );
    assert_eq!(
        parse_slash_command("/compact"),
        Some(CommandAction::Compact)
    );
    assert_eq!(parse_slash_command("/quit"), Some(CommandAction::Quit));
    assert_eq!(parse_slash_command("/exit"), Some(CommandAction::Quit));
    assert_eq!(
        parse_slash_command("/session"),
        Some(CommandAction::ShowSession)
    );
    assert_eq!(
        parse_slash_command("/name test session"),
        Some(CommandAction::SetName("test session".to_string()))
    );
    assert_eq!(
        parse_slash_command("/tree 123"),
        Some(CommandAction::SwitchTreeBranch("123".to_string()))
    );
    assert_eq!(
        parse_slash_command("/fork 123"),
        Some(CommandAction::Fork("123".to_string()))
    );
    assert_eq!(
        parse_slash_command("/clone"),
        Some(CommandAction::CloneSession)
    );
    assert_eq!(
        parse_slash_command("/skill my_skill"),
        Some(CommandAction::InvokeSkill("my_skill".to_string()))
    );
    assert_eq!(
        parse_slash_command("/prompt tpl arg1 arg2"),
        Some(CommandAction::PromptTemplate("tpl arg1 arg2".to_string()))
    );

    // Unknown commands
    assert_eq!(
        parse_slash_command("/plan"),
        Some(CommandAction::Unknown("plan".to_string()))
    );
    assert_eq!(
        parse_slash_command("/todos"),
        Some(CommandAction::Unknown("todos".to_string()))
    );

    // Whitespace handling
    assert_eq!(
        parse_slash_command("  /model   gpt-4o  "),
        Some(CommandAction::SwitchModel("gpt-4o".to_string()))
    );
    assert_eq!(
        parse_slash_command("\n\t/compact\n"),
        Some(CommandAction::Compact)
    );

    // Invalid inputs
    assert_eq!(parse_slash_command("not a slash command"), None);
    assert_eq!(parse_slash_command(""), None);
    assert_eq!(parse_slash_command("  "), None);
    assert_eq!(parse_slash_command("/"), None);
}
