use crate::ui::{AppState, RunStatus};
use threadlane_agent::{PlanItemStatus, ReasoningEffort};
use threadlane_coding_agent::CodingAgent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandDescription {
    pub label: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

const COMMAND_DESCRIPTIONS: [CommandDescription; 8] = [
    CommandDescription {
        label: "/model",
        usage: "/model [provider/model]",
        description: "Show or set the active model.",
    },
    CommandDescription {
        label: "/models",
        usage: "/models",
        description: "List available models.",
    },
    CommandDescription {
        label: "/reasoning",
        usage: "/reasoning [off|minimal|low|medium|high|xhigh]",
        description: "Show or set reasoning effort.",
    },
    CommandDescription {
        label: "/plan",
        usage: "/plan",
        description: "Show the current plan.",
    },
    CommandDescription {
        label: "/clear",
        usage: "/clear",
        description: "Clear the transcript.",
    },
    CommandDescription {
        label: "/session",
        usage: "/session",
        description: "Show session details.",
    },
    CommandDescription {
        label: "/help",
        usage: "/help",
        description: "List commands.",
    },
    CommandDescription {
        label: "/quit",
        usage: "/quit",
        description: "Exit the CLI.",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    ShowModel,
    SetModel(String),
    Models,
    ShowReasoning,
    SetReasoning(ReasoningEffort),
    Plan,
    Clear,
    Session,
    Help,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandError {
    Unknown(String),
    InvalidArguments(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(command) => write!(formatter, "Unknown command: /{command}"),
            Self::InvalidArguments(command) => {
                write!(formatter, "Invalid arguments for /{command}")
            }
        }
    }
}

pub(crate) fn command_descriptions() -> &'static [CommandDescription] {
    &COMMAND_DESCRIPTIONS
}

pub(crate) fn filter_command_labels(query: &str) -> Vec<String> {
    let query = query.trim().trim_start_matches('/').to_ascii_lowercase();
    command_descriptions()
        .iter()
        .filter(|command| {
            query.is_empty() || command.label[1..].to_ascii_lowercase().starts_with(&query)
        })
        .map(|command| command.label.to_string())
        .collect()
}

pub(crate) fn filter_model_labels(query: &str, models: &[String]) -> Vec<String> {
    let query = query.trim().to_ascii_lowercase();
    models
        .iter()
        .filter(|model| query.is_empty() || model.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

pub(crate) fn parse_command(input: &str) -> Result<Command, CommandError> {
    let mut parts = input
        .trim()
        .strip_prefix('/')
        .ok_or_else(|| CommandError::Unknown(input.trim().to_string()))?
        .split_whitespace();
    let command = parts.next().unwrap_or_default();
    let argument = parts.next();
    if parts.next().is_some() {
        return Err(CommandError::InvalidArguments(command.into()));
    }

    match (command, argument) {
        ("model", None) => Ok(Command::ShowModel),
        ("model", Some(model)) if !model.is_empty() => Ok(Command::SetModel(model.into())),
        ("models", None) => Ok(Command::Models),
        ("reasoning", None) => Ok(Command::ShowReasoning),
        ("reasoning", Some(level)) => ReasoningEffort::from_label(level)
            .map(Command::SetReasoning)
            .ok_or_else(|| CommandError::InvalidArguments("reasoning".into())),
        ("plan", None) => Ok(Command::Plan),
        ("clear", None) => Ok(Command::Clear),
        ("session", None) => Ok(Command::Session),
        ("help", None) => Ok(Command::Help),
        ("quit", None) => Ok(Command::Quit),
        ("model", _) => Err(CommandError::InvalidArguments("model".into())),
        (known, Some(_))
            if matches!(
                known,
                "models" | "plan" | "clear" | "session" | "help" | "quit"
            ) =>
        {
            Err(CommandError::InvalidArguments(known.into()))
        }
        (unknown, _) => Err(CommandError::Unknown(unknown.into())),
    }
}

pub(crate) enum CommandResult {
    Message(String),
    Quit,
}

pub(crate) struct CommandContext<'a> {
    pub agent: &'a mut CodingAgent,
    pub state: &'a mut AppState,
}

fn running(state: &AppState, command: &Command) -> bool {
    matches!(state.status, RunStatus::Running)
        && matches!(
            command,
            Command::SetModel(_) | Command::SetReasoning(_) | Command::Clear
        )
}

fn format_plan(state: &AppState) -> String {
    let Some(plan) = &state.plan else {
        return "No active plan.".into();
    };
    if plan.items.is_empty() {
        return "No active plan.".into();
    }
    let mut lines = plan.explanation.clone().into_iter().collect::<Vec<_>>();
    lines.extend(plan.items.iter().map(|item| {
        let marker = match item.status {
            PlanItemStatus::Pending => "[ ]",
            PlanItemStatus::InProgress => "[>]",
            PlanItemStatus::Completed => "[x]",
        };
        format!("{marker} {}", item.step)
    }));
    lines.join("\n")
}

pub(crate) async fn execute_command(
    context: &mut CommandContext<'_>,
    command: Command,
) -> CommandResult {
    if running(context.state, &command) {
        return CommandResult::Message(
            "Cannot change settings while generation is running.".into(),
        );
    }

    match command {
        Command::ShowModel => CommandResult::Message(format!("Current model: {}", context.state.model)),
        Command::SetModel(model) => match context.agent.set_model(model.clone()).await {
            Ok(()) => {
                context.state.model = model.clone();
                CommandResult::Message(format!("Switched model to: {model}"))
            }
            Err(error) => CommandResult::Message(error),
        },
        Command::Models => {
            let models = context.agent.available_models().await;
            CommandResult::Message(format!("Available models:\n{}", models.join("\n")))
        }
        Command::ShowReasoning => CommandResult::Message(format!(
            "Current reasoning effort: {}",
            context.state.reasoning_effort.label()
        )),
        Command::SetReasoning(effort) => {
            context.agent.set_reasoning_effort(effort).await;
            context.state.reasoning_effort = effort;
            CommandResult::Message(format!("Reasoning effort: {}", effort.label()))
        }
        Command::Plan => CommandResult::Message(format_plan(context.state)),
        Command::Clear => {
            context.state.messages.clear();
            context.state.streaming = None;
            context.state.scroll = 0;
            context.state.follow_tail = true;
            CommandResult::Message("Transcript cleared.".into())
        }
        Command::Session => CommandResult::Message(format!(
            "Workspace: {}\nModel: {}\nReasoning: {}",
            context.state.work_dir,
            context.state.model,
            context.state.reasoning_effort.label()
        )),
        Command::Help => CommandResult::Message(
            format!(
                "Commands: {}",
                command_descriptions()
                    .iter()
                    .map(|command| command.usage)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        Command::Quit => CommandResult::Quit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_agent::ReasoningEffort;
    use threadlane_coding_agent::CodingAgentOptions;

    #[test]
    fn filters_command_labels_from_known_commands() {
        assert_eq!(
            filter_command_labels("mo"),
            vec!["/model".to_string(), "/models".to_string()]
        );
        assert_eq!(filter_command_labels("rea"), vec!["/reasoning".to_string()]);
        assert!(filter_command_labels("zzz").is_empty());
    }

    #[test]
    fn filters_model_labels_case_insensitively() {
        let models = vec![
            "gpt-4o".to_string(),
            "antigravity/gemini".to_string(),
            "GPT-5".to_string(),
        ];

        assert_eq!(
            filter_model_labels("gPt", &models),
            vec!["gpt-4o".to_string(), "GPT-5".to_string()]
        );
        assert_eq!(
            filter_model_labels("gravity", &models),
            vec!["antigravity/gemini".to_string()]
        );
        assert!(filter_model_labels("claude", &models).is_empty());
    }

    #[test]
    fn parses_model_and_reasoning_commands() {
        assert_eq!(parse_command("/model").unwrap(), Command::ShowModel);
        assert_eq!(
            parse_command("/model antigravity/gemini").unwrap(),
            Command::SetModel("antigravity/gemini".into())
        );
        assert_eq!(
            parse_command("/reasoning high").unwrap(),
            Command::SetReasoning(ReasoningEffort::High)
        );
        assert_eq!(parse_command("/help").unwrap(), Command::Help);
    }

    #[test]
    fn rejects_unknown_commands_and_extra_model_arguments() {
        assert!(matches!(
            parse_command("/wat"),
            Err(CommandError::Unknown(_))
        ));
        assert!(parse_command("/model a b").is_err());
    }

    #[tokio::test]
    async fn model_command_persists_the_provider_prefixed_model() {
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: String::new(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: std::env::current_dir().unwrap(),
            session_file: None,
            system_prompt: Default::default(),
        });
        let mut state = AppState::test_state();
        let result = execute_command(
            &mut CommandContext {
                agent: &mut agent,
                state: &mut state,
            },
            Command::SetModel("antigravity/gemini".into()),
        )
        .await;

        assert!(
            matches!(result, CommandResult::Message(message) if message == "Switched model to: antigravity/gemini")
        );
        assert_eq!(state.model, "antigravity/gemini");
        assert_eq!(
            agent.session_tree.model.as_deref(),
            Some("antigravity/gemini")
        );
    }

    #[tokio::test]
    async fn rejects_mutating_commands_while_running() {
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: String::new(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: std::env::current_dir().unwrap(),
            session_file: None,
            system_prompt: Default::default(),
        });
        let mut state = AppState::test_state_generating();
        let result = execute_command(
            &mut CommandContext {
                agent: &mut agent,
                state: &mut state,
            },
            Command::SetReasoning(ReasoningEffort::High),
        )
        .await;

        assert!(matches!(result, CommandResult::Message(message) if message.contains("running")));
        assert_eq!(state.reasoning_effort, ReasoningEffort::Medium);
    }
}
