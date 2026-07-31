use crate::{
    input::{self, InputEvent},
    resolve_credentials, tui,
    ui::{reduce_agent_event, AppState, MessageType, RunStatus, TranscriptMessage},
};
use crossterm::event;
use std::{path::PathBuf, sync::Arc, time::Duration};
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{CodingAgent, CodingAgentCancellation, CodingAgentOptions};
use tokio::sync::Mutex;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Submit(String),
    Cancel,
    Quit,
    None,
}

pub(crate) fn dispatch_input(state: &mut AppState, input: InputEvent) -> Action {
    match input {
        InputEvent::Submit if !matches!(state.status, RunStatus::Running) => {
            Action::Submit(state.composer.clone())
        }
        InputEvent::CancelOrQuit => {
            if matches!(state.status, RunStatus::Running) {
                Action::Cancel
            } else {
                Action::Quit
            }
        }
        InputEvent::Character(character) if !matches!(state.status, RunStatus::Running) => {
            state.composer.push(character);
            Action::None
        }
        InputEvent::Backspace if !matches!(state.status, RunStatus::Running) => {
            state.composer.pop();
            Action::None
        }
        InputEvent::ScrollUp => {
            state.scroll_up();
            Action::None
        }
        InputEvent::ScrollDown => {
            state.scroll_down();
            Action::None
        }
        InputEvent::Resize | InputEvent::Submit | InputEvent::Character(_) | InputEvent::Backspace => Action::None,
    }
}

pub(crate) fn spawn_prompt(
    agent: Arc<Mutex<CodingAgent>>,
    cancellation: CodingAgentCancellation,
    prompt: String,
) -> Result<(), String> {
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let completion = cancellation.clone();
    let task = tokio::spawn(async move {
        let Ok(run_id) = start_rx.await else { return };
        let mut agent = agent.lock().await;
        let _ = agent.handle_input_with_images(&prompt, vec![]).await;
        completion.finish_active_run(run_id);
    });
    let run_id = cancellation.track_active_run(task.abort_handle())?;
    start_tx.send(run_id).map_err(|_| "Generation ended before it could start".to_string())
}

pub(crate) async fn run_tui(
    work_dir: PathBuf,
    model: String,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let cancellation = agent.cancellation_handle();
    let agent = Arc::new(Mutex::new(agent));

    loop {
        terminal.draw(|frame| crate::ui::render(frame, &state))?;

        if event::poll(Duration::from_millis(50))? {
            if let Some(input) = input::map_event(event::read()?) {
                match dispatch_input(&mut state, input) {
                    Action::Submit(submission) if !submission.trim().is_empty() => {
                        state.composer.clear();
                        state.messages.push(TranscriptMessage {
                            msg_type: MessageType::User,
                            content: submission.clone(),
                        });
                        state.begin_generation();
                        if let Err(error) = spawn_prompt(Arc::clone(&agent), cancellation.clone(), submission) {
                            reduce_agent_event(&mut state, AgentEvent::AgentError { error });
                        }
                    }
                    Action::Cancel => {
                        if let Err(error) = cancellation.cancel() {
                            reduce_agent_event(&mut state, AgentEvent::AgentError { error });
                        }
                    }
                    Action::Quit => break,
                    Action::Submit(_) | Action::None => {}
                }
            }
        }

        while let Ok(event) = event_rx.try_recv() {
            reduce_agent_event(&mut state, event);
        }
    }

    if matches!(state.status, RunStatus::Running) {
        if let Err(error) = cancellation.cancel() {
            reduce_agent_event(&mut state, AgentEvent::AgentError { error });
        }
    }
    while let Ok(event) = event_rx.try_recv() {
        reduce_agent_event(&mut state, event);
    }
    terminal.restore()?;
    Ok(())
}
