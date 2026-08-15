use threadlane_agent::{SessionTree, UnifiedAgent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: String,
}

/// Built-in slash commands handled by the coding agent.
pub fn builtin_commands() -> Vec<SlashCommandInfo> {
    [
        ("model", "Switch model, or show the current one"),
        ("compact", "Compact the conversation context"),
        ("session", "Show session info"),
        ("name", "Name this session"),
        ("tree", "Switch session tree branch"),
        ("fork", "Fork a session tree branch"),
        ("clone", "Clone the active session tree"),
        ("skill", "Load a discovered skill by ID"),
        (
            "subagent",
            "Delegate tasks to subagents in parallel or sequentially",
        ),
        ("task", "Run a prompt as a background task"),
        ("quit", "Quit threadlane agent"),
    ]
    .into_iter()
    .map(|(name, description)| SlashCommandInfo {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    SwitchModel(String),
    Compact,
    ShowSession,
    SetName(String),
    SwitchTreeBranch(String),
    Fork(String),
    CloneSession,
    InvokeSkill(String),
    PromptTemplate(String),

    Quit,
    Unknown(String),
}

pub fn parse_slash_command(input: &str) -> Option<CommandAction> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let mut parts = trimmed[1..].split_whitespace();
    let cmd = parts.next()?;
    let arg = parts.collect::<Vec<&str>>().join(" ");

    match cmd {
        "model" => Some(CommandAction::SwitchModel(arg)),
        "compact" => Some(CommandAction::Compact),
        "session" => Some(CommandAction::ShowSession),
        "name" => Some(CommandAction::SetName(arg)),
        "tree" => Some(CommandAction::SwitchTreeBranch(arg)),
        "fork" => Some(CommandAction::Fork(arg)),
        "clone" => Some(CommandAction::CloneSession),
        "skill" => Some(CommandAction::InvokeSkill(arg)),
        "prompt" => Some(CommandAction::PromptTemplate(arg)),

        "quit" | "exit" => Some(CommandAction::Quit),
        other => Some(CommandAction::Unknown(other.to_string())),
    }
}

pub async fn execute_slash_command(
    action: CommandAction,
    agent: &mut UnifiedAgent,
    session_tree: &mut SessionTree,
) -> String {
    match action {
        CommandAction::SwitchModel(new_model) => {
            if new_model.is_empty() {
                let st = agent.get_state().await;
                format!("Current model: {}", st.model)
            } else if let Err(error) = session_tree.set_model(new_model.clone()) {
                format!("Could not persist model switch: {error}")
            } else {
                let mut st = agent.turn.lock().await;
                st.model = new_model.clone();
                format!("Switched model to: {}", new_model)
            }
        }
        CommandAction::Compact => {
            if !agent.compact_history(None).await {
                "Nothing to compact yet.".to_string()
            } else {
                let state = agent.get_state().await;
                session_tree.replace_active_branch(state.messages);
                "Context compacted in the current session.".to_string()
            }
        }
        CommandAction::ShowSession => {
            let st = agent.get_state().await;
            format!(
                "Session ID: {}\nName: {}\nMessage Count: {}\nModel: {}",
                session_tree.session_id,
                session_tree.name.as_deref().unwrap_or("unnamed"),
                st.messages.len(),
                st.model,
            )
        }
        CommandAction::SetName(name) => {
            session_tree.name = Some(name.clone());
            format!("Session name set to: {}", name)
        }
        CommandAction::SwitchTreeBranch(node_id) => {
            if session_tree.switch_active_node(&node_id) {
                let branch_msgs = session_tree.get_active_branch_messages();
                let mut st = agent.turn.lock().await;
                st.messages = branch_msgs;
                format!("Switched session tree to node: {}", node_id)
            } else {
                format!("Node ID not found in session tree: {}", node_id)
            }
        }
        CommandAction::Fork(node_id) => {
            if let Some(forked) = session_tree.fork_branch(&node_id) {
                format!(
                    "Forked session tree successfully into ID: {}",
                    forked.session_id
                )
            } else {
                format!("Failed to fork. Node ID not found: {}", node_id)
            }
        }
        CommandAction::CloneSession => {
            let mut cloned = session_tree.clone();
            cloned.session_id = format!("{}_clone", session_tree.session_id);
            format!("Cloned active session tree into ID: {}", cloned.session_id)
        }
        CommandAction::InvokeSkill(skill) => format!("Invoking skill: {}", skill),
        CommandAction::PromptTemplate(tmpl) => format!("Prompt template: {}", tmpl),

        CommandAction::Quit => "Quitting threadlane agent.".to_string(),
        CommandAction::Unknown(cmd) => format!("Unknown command: /{}", cmd),
    }
}
