mod tui;
mod ui;

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{CodingAgent, CodingAgentOptions};
use tokio::sync::Mutex;
use ui::{AppState, MessageType, TranscriptMessage};

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
    run_tui(canonical_work_dir, args.model).await?;
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

async fn run_tui(work_dir: PathBuf, model: String) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = tui::init()?;
    let mut state = AppState::new(model.clone(), work_dir.display().to_string());

    let (api_key, account_id) = resolve_credentials();
    let agent = CodingAgent::new(CodingAgentOptions {
        api_key,
        account_id,
        model,
        work_dir,
        session_file: None,
        system_prompt: Default::default(),
    });

    let mut event_rx = agent.subscribe();
    let shared_agent = Arc::new(Mutex::new(agent));

    loop {
        terminal.draw(|f| ui::render(f, &state))?;

        // Poll for key inputs
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Enter => {
                        if !state.input.trim().is_empty() && !state.is_generating {
                            let user_prompt = state.input.clone();
                            state.input.clear();
                            state.messages.push(TranscriptMessage {
                                msg_type: MessageType::User,
                                content: user_prompt.clone(),
                            });
                            state.messages.push(TranscriptMessage {
                                msg_type: MessageType::Assistant,
                                content: String::new(),
                            });
                            state.is_generating = true;

                            let agent_ref = Arc::clone(&shared_agent);
                            tokio::spawn(async move {
                                let mut guard = agent_ref.lock().await;
                                let _ = guard.handle_input_with_images(&user_prompt, vec![]).await;
                            });
                        }
                    }
                    KeyCode::Char(c) => {
                        if !state.is_generating {
                            state.input.push(c);
                        }
                    }
                    KeyCode::Backspace => {
                        if !state.is_generating {
                            state.input.pop();
                        }
                    }
                    KeyCode::Up => {
                        if state.scroll > 0 {
                            state.scroll -= 1;
                        }
                    }
                    KeyCode::Down => {
                        state.scroll += 1;
                    }
                    _ => {}
                }
            }
        }

        // Drain pending agent streaming events
        while let Ok(event) = event_rx.try_recv() {
            match event {
                AgentEvent::MessageUpdate {
                    text_delta,
                    reasoning_delta,
                    ..
                } => {
                    if let Some(delta) = text_delta {
                        if let Some(last_msg) = state.messages.last_mut() {
                            if matches!(last_msg.msg_type, MessageType::Assistant) {
                                last_msg.content.push_str(&delta);
                            }
                        }
                    }
                    if let Some(r_delta) = reasoning_delta {
                        if let Some(last_msg) = state.messages.last() {
                            if !matches!(last_msg.msg_type, MessageType::Thinking) {
                                state.messages.push(TranscriptMessage {
                                    msg_type: MessageType::Thinking,
                                    content: String::new(),
                                });
                            }
                        }
                        if let Some(last_msg) = state.messages.last_mut() {
                            last_msg.content.push_str(&r_delta);
                        }
                    }
                }
                AgentEvent::ToolExecutionStart {
                    tool_call_id, name, ..
                } => {
                    state.messages.push(TranscriptMessage {
                        msg_type: MessageType::ToolCall(name),
                        content: format!("Tool ID: {tool_call_id}"),
                    });
                }
                AgentEvent::AgentEnd { .. } => {
                    state.is_generating = false;
                }
                _ => {}
            }
        }
    }

    tui::restore()?;
    Ok(())
}

fn resolve_credentials() -> (String, Option<String>) {
    let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
    let account_id = env::var("CHATGPT_ACCOUNT_ID").ok();
    (api_key, account_id)
}
