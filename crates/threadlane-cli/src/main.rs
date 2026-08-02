mod commands;
mod input;
mod runtime;
mod tui;
mod ui;

use clap::Parser;
#[cfg(test)]
use runtime::{dispatch_input, Action};
use std::env;
use std::path::PathBuf;
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{CodingAgent, CodingAgentOptions};
#[cfg(test)]
use ui::{AppState, RunStatus};

#[derive(Parser, Debug)]
#[command(author, version, about = "Threadlane Terminal CLI & Ratatui TUI Runner", long_about = None)]
struct CliArgs {
    /// Optional single prompt for one-shot execution (streams directly to stdout)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Model to use for generation
    #[arg(short, long, default_value = "gpt-4o")]
    model: String,

    /// Working directory for the agent
    #[arg(short, long)]
    dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = CliArgs::parse();
    let work_dir = args
        .dir
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let canonical_work_dir = std::fs::canonicalize(&work_dir).unwrap_or(work_dir);

    // If one-shot prompt is provided, run in headless mode streaming to stdout
    if let Some(prompt) = args.prompt {
        run_headless(canonical_work_dir, args.model, prompt).await?;
        return Ok(());
    }

    // Otherwise launch full Ratatui TUI interactive mode
    runtime::run_tui(canonical_work_dir, args.model).await?;
    Ok(())
}

async fn run_headless(
    work_dir: PathBuf,
    model: String,
    prompt: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let (api_key, account_id) = resolve_credentials();
    let mut agent = CodingAgent::new(CodingAgentOptions {
        api_key,
        account_id,
        model,
        work_dir,
        session_file: None,
        system_prompt: Default::default(),
    });

    let mut event_rx = agent.subscribe();
    let prompt_clone = prompt.clone();

    tokio::spawn(async move {
        let _ = agent.handle_input_with_images(&prompt_clone, vec![]).await;
    });

    while let Ok(event) = event_rx.recv().await {
        match event {
            AgentEvent::MessageUpdate {
                text_delta: Some(delta),
                ..
            } => {
                print!("{delta}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            AgentEvent::AgentEnd { .. } => {
                println!();
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

pub(crate) fn resolve_credentials() -> (String, Option<String>) {
    let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
    let account_id = env::var("CHATGPT_ACCOUNT_ID").ok();
    (api_key, account_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_submits_only_when_idle_and_composer_is_nonempty() {
        let mut state = AppState::test_state();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::Submit),
            Action::Submit("".into())
        );
        state.composer = "inspect the project".into();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::Submit),
            Action::Submit("inspect the project".into())
        );
        state.begin_generation();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::Submit),
            Action::None
        );
    }

    #[test]
    fn escape_cancels_generation_before_quitting() {
        let mut state = AppState::test_state_generating();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::CancelOrQuit),
            Action::Cancel
        );
        state.status = RunStatus::Idle;
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::CancelOrQuit),
            Action::Quit
        );
    }
}
