use crate::capabilities::CapabilityCatalog;
use std::path::Path;
use threadlane_agent::{ProviderRunExecutor, SessionTree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: String,
}

/// Built-in slash commands handled by the coding agent.
pub fn builtin_commands() -> Vec<SlashCommandInfo> {
    [
        ("model", "Switch model, or show the current one"),
        (
            "advisor",
            "Toggle or configure the advisor reviewer model (/advisor on|off|status|model <id>)",
        ),
        (
            "plan",
            "Create or refine an implementation plan using the plan model (/plan <objective>)",
        ),
        (
            "roles",
            "View or configure model roles (task, plan, advisor)",
        ),
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

/// All slash commands available to the user, including built-ins and
/// commands contributed by active extensions.
pub fn available_slash_commands(project_root: Option<&Path>) -> Vec<SlashCommandInfo> {
    let mut commands = builtin_commands();
    let catalog = CapabilityCatalog::discover(project_root);
    for record in catalog.extensions() {
        if !record.is_effective() || !record.is_enabled() {
            continue;
        }
        if let Ok(ext) =
            threadlane_wasi::WasiExtension::load_from_file_requiring_manifest(record.module_path())
        {
            for cmd in ext.manifest.commands {
                if !commands.iter().any(|c| c.name == cmd.name) {
                    commands.push(SlashCommandInfo {
                        name: cmd.name,
                        description: cmd.description,
                    });
                }
            }
        }
    }
    commands
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    SwitchModel(String),
    Advisor(String),
    Plan(String),
    Roles(String),
    Compact,
    ShowSession,
    SetName(String),
    SwitchTreeBranch(String),
    Fork(String),
    CloneSession,
    InvokeSkill(String),
    PromptTemplate(String),
    Subagent(String),
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
        "advisor" => Some(CommandAction::Advisor(arg)),
        "plan" => Some(CommandAction::Plan(arg)),
        "roles" => Some(CommandAction::Roles(arg)),
        "compact" => Some(CommandAction::Compact),
        "session" => Some(CommandAction::ShowSession),
        "name" => Some(CommandAction::SetName(arg)),
        "tree" => Some(CommandAction::SwitchTreeBranch(arg)),
        "fork" => Some(CommandAction::Fork(arg)),
        "clone" => Some(CommandAction::CloneSession),
        "skill" => Some(CommandAction::InvokeSkill(arg)),
        "prompt" => Some(CommandAction::PromptTemplate(arg)),
        "subagent" => Some(CommandAction::Subagent(arg)),

        "quit" | "exit" => Some(CommandAction::Quit),
        other => Some(CommandAction::Unknown(other.to_string())),
    }
}

pub async fn execute_slash_command(
    action: CommandAction,
    agent: &mut ProviderRunExecutor,
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
                {
                    let mut st = agent.turn.lock().await;
                    st.model = new_model.clone();
                }
                let mut roles = agent.model_roles().clone();
                roles.task = Some(new_model.clone());
                agent.set_model_roles(roles);
                format!("Switched model to: {}", new_model)
            }
        }
        CommandAction::Advisor(arg) => {
            let mut roles = agent.model_roles().clone();
            let trimmed = arg.trim();
            if trimmed.is_empty() || trimmed == "status" {
                let status = if roles.advisor_enabled {
                    "ENABLED"
                } else {
                    "DISABLED"
                };
                let model = roles.advisor.as_deref().unwrap_or("inherit main");
                format!("Advisor status: {status}\nAdvisor model: {model}\nUsage: /advisor on | off | model <model-id>")
            } else if trimmed == "on" || trimmed == "enable" {
                roles.advisor_enabled = true;
                agent.set_model_roles(roles);
                "Advisor reviewer turned ON (watching every turn).".to_string()
            } else if trimmed == "off" || trimmed == "disable" {
                roles.advisor_enabled = false;
                agent.set_model_roles(roles);
                "Advisor reviewer turned OFF.".to_string()
            } else if let Some(model_id) = trimmed.strip_prefix("model ") {
                let model_id = model_id.trim().to_string();
                roles.advisor = Some(model_id.clone());
                agent.set_model_roles(roles);
                format!("Advisor model set to: {model_id}")
            } else {
                format!("Unknown advisor subcommand: {trimmed}. Use: /advisor on | off | status | model <id>")
            }
        }
        CommandAction::Roles(arg) => {
            let mut roles = agent.model_roles().clone();
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                let main_model = agent.get_state().await.model;
                format!(
                    "Model Roles:\n  Task (execution): {}\n  Plan (architecture): {}\n  Advisor (reviewer): {} [{}]\n\nUsage: /roles plan=<model> | task=<model> | advisor=<model>",
                    roles.resolve_task(&main_model),
                    roles.resolve_plan(&main_model),
                    roles.resolve_advisor(&main_model),
                    if roles.advisor_enabled { "active" } else { "inactive" }
                )
            } else if let Some((key, val)) = trimmed.split_once('=') {
                let key = key.trim().to_lowercase();
                let val = val.trim().to_string();
                match key.as_str() {
                    "task" => {
                        roles.task = Some(val.clone());
                        agent.set_model_roles(roles);
                        format!("Task model role set to: {val}")
                    }
                    "plan" => {
                        roles.plan = Some(val.clone());
                        agent.set_model_roles(roles);
                        format!("Plan model role set to: {val}")
                    }
                    "advisor" => {
                        roles.advisor = Some(val.clone());
                        agent.set_model_roles(roles);
                        format!("Advisor model role set to: {val}")
                    }
                    other => {
                        format!("Unknown model role: {other}. Available roles: task, plan, advisor")
                    }
                }
            } else {
                format!("Usage: /roles plan=<model> | task=<model> | advisor=<model>")
            }
        }
        CommandAction::Plan(objective) => {
            if objective.trim().is_empty() {
                "Usage: /plan <task objective or prompt> to generate a structured implementation plan.".to_string()
            } else {
                format!("Generating implementation plan for: {}", objective.trim())
            }
        }
        CommandAction::Compact => {
            if !agent.compact_history(None).await {
                "Nothing to compact yet.".to_string()
            } else if session_tree.file_path.is_none() {
                // Legacy in-memory sessions have no canonical journal yet.
                let state = agent.get_state().await;
                session_tree.replace_active_branch(state.messages);
                "Context compacted in the current session.".to_string()
            } else {
                "Context compaction requires the durable session harness.".to_string()
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
                if session_tree.file_path.is_none() {
                    let branch_msgs = session_tree.get_active_branch_messages();
                    let mut st = agent.turn.lock().await;
                    st.messages = branch_msgs;
                    format!("Switched session tree to node: {}", node_id)
                } else {
                    "Branch switching requires the durable session harness.".to_string()
                }
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
        CommandAction::Subagent(task) => {
            let trimmed = task.trim();
            if trimmed.is_empty() {
                "Usage: /subagent <task description>".to_string()
            } else {
                format!("Delegating subagent task: {trimmed}")
            }
        }

        CommandAction::Quit => "Quitting threadlane agent.".to_string(),
        CommandAction::Unknown(cmd) => format!("Unknown command: /{}", cmd),
    }
}
