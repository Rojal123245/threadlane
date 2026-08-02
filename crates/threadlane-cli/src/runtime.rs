use crate::{
    commands::{
        execute_command, filter_command_labels, filter_model_labels, parse_command, CommandContext,
        CommandResult,
    },
    input::{self, InputEvent},
    resolve_credentials, tui,
    ui::{reduce_agent_event, AppState, CompletionMode, MessageType, RunStatus, TranscriptMessage},
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
    Message(String),
    None,
}

pub(crate) fn dispatch_input(state: &mut AppState, input: InputEvent) -> Action {
    dispatch_input_with_models(state, input, &[])
}

fn needs_model_catalog(state: &AppState, input: &InputEvent) -> bool {
    if matches!(state.status, RunStatus::Running) {
        return false;
    }
    let next = match input {
        InputEvent::Character(character) => {
            let mut next = state.composer.clone();
            next.push(*character);
            next
        }
        InputEvent::Backspace => {
            let mut next = state.composer.clone();
            next.pop();
            next
        }
        InputEvent::Submit | InputEvent::Tab => state.composer.clone(),
        _ => return false,
    };
    matches!(state.completion.mode, Some(CompletionMode::Model))
        || next.trim() == "/model"
        || next.starts_with("/model ")
}

fn show_model_completion(state: &mut AppState, models: &[String]) -> Action {
    let query = state
        .composer
        .strip_prefix("/model")
        .unwrap_or_default()
        .trim();
    let candidates = filter_model_labels(query, models);
    if candidates.is_empty() {
        state.close_completion();
        Action::Message("No available models found; keeping current model.".into())
    } else {
        state.show_completion(CompletionMode::Model, candidates);
        Action::None
    }
}

fn refresh_completion(state: &mut AppState, models: &[String]) -> Action {
    if state.composer == "/" || state.composer.starts_with('/') && !state.composer.contains(' ') {
        state.show_completion(
            CompletionMode::Command,
            filter_command_labels(&state.composer),
        );
        return Action::None;
    }
    if state.composer.trim() == "/model" || state.composer.starts_with("/model ") {
        return show_model_completion(state, models);
    }
    state.close_completion();
    Action::None
}

fn accept_completion(state: &mut AppState) {
    let Some(candidate) = state
        .completion
        .candidates
        .get(state.completion.selected)
        .cloned()
    else {
        state.close_completion();
        return;
    };
    state.composer = match state.completion.mode {
        Some(CompletionMode::Model) => format!("/model {candidate}"),
        _ => candidate,
    };
    state.close_completion();
}

pub(crate) fn dispatch_input_with_models(
    state: &mut AppState,
    input: InputEvent,
    models: &[String],
) -> Action {
    if state.completion.visible {
        match input {
            InputEvent::Submit | InputEvent::Tab => {
                accept_completion(state);
                return Action::None;
            }
            InputEvent::CancelOrQuit => {
                state.close_completion();
                return Action::None;
            }
            InputEvent::Previous => {
                state.select_previous_completion();
                return Action::None;
            }
            InputEvent::Next => {
                state.select_next_completion();
                return Action::None;
            }
            InputEvent::Character(character) if !matches!(state.status, RunStatus::Running) => {
                state.composer.push(character);
                return refresh_completion(state, models);
            }
            InputEvent::Backspace if !matches!(state.status, RunStatus::Running) => {
                state.composer.pop();
                return refresh_completion(state, models);
            }
            _ => {}
        }
    }

    match input {
        InputEvent::Submit if state.composer.trim() == "/model" => {
            show_model_completion(state, models)
        }
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
            refresh_completion(state, models)
        }
        InputEvent::Backspace if !matches!(state.status, RunStatus::Running) => {
            state.composer.pop();
            refresh_completion(state, models)
        }
        InputEvent::Previous => {
            state.scroll_up();
            Action::None
        }
        InputEvent::Next => {
            state.scroll_down();
            Action::None
        }
        InputEvent::Resize
        | InputEvent::Submit
        | InputEvent::Tab
        | InputEvent::Character(_)
        | InputEvent::Backspace => Action::None,
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
    start_tx
        .send(run_id)
        .map_err(|_| "Generation ended before it could start".to_string())
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
    let mut available_models: Option<Vec<String>> = None;

    loop {
        terminal.draw(|frame| crate::ui::render(frame, &state))?;

        if event::poll(Duration::from_millis(50))? {
            if let Some(input) = input::map_event(event::read()?) {
                if needs_model_catalog(&state, &input) && available_models.is_none() {
                    let models = agent.lock().await.available_models().await;
                    available_models = Some(if models.is_empty() {
                        vec![state.model.clone()]
                    } else {
                        models
                    });
                }
                match dispatch_input_with_models(
                    &mut state,
                    input,
                    available_models.as_deref().unwrap_or(&[]),
                ) {
                    Action::Submit(submission) if !submission.trim().is_empty() => {
                        state.composer.clear();
                        if submission.starts_with('/') {
                            let result = match parse_command(&submission) {
                                Ok(command) => {
                                    let mut agent = agent.lock().await;
                                    execute_command(
                                        &mut CommandContext {
                                            agent: &mut agent,
                                            state: &mut state,
                                        },
                                        command,
                                    )
                                    .await
                                }
                                Err(error) => CommandResult::Message(error.to_string()),
                            };
                            match result {
                                CommandResult::Message(content) => {
                                    state.messages.push(TranscriptMessage {
                                        msg_type: MessageType::Assistant,
                                        content,
                                    })
                                }
                                CommandResult::Quit => break,
                            }
                        } else {
                            state.messages.push(TranscriptMessage {
                                msg_type: MessageType::User,
                                content: submission.clone(),
                            });
                            state.begin_generation();
                            if let Err(error) =
                                spawn_prompt(Arc::clone(&agent), cancellation.clone(), submission)
                            {
                                reduce_agent_event(&mut state, AgentEvent::AgentError { error });
                            }
                        }
                    }
                    Action::Cancel => {
                        if let Err(error) = cancellation.cancel() {
                            reduce_agent_event(&mut state, AgentEvent::AgentError { error });
                        }
                    }
                    Action::Quit => break,
                    Action::Message(content) => state.messages.push(TranscriptMessage {
                        msg_type: MessageType::Assistant,
                        content,
                    }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_keys_insert_navigate_and_cancel_commands() {
        let mut state = AppState::test_state();

        assert_eq!(
            dispatch_input(&mut state, InputEvent::Character('/')),
            Action::None
        );
        assert_eq!(state.completion.mode, Some(CompletionMode::Command));
        assert_eq!(state.completion.candidates[0], "/model");

        assert_eq!(dispatch_input(&mut state, InputEvent::Next), Action::None);
        assert_eq!(state.completion.selected, 1);
        assert_eq!(
            dispatch_input(&mut state, InputEvent::Previous),
            Action::None
        );
        assert_eq!(state.completion.selected, 0);

        assert_eq!(dispatch_input(&mut state, InputEvent::Tab), Action::None);
        assert_eq!(state.composer, "/model");
        assert!(!state.completion.visible);

        assert_eq!(
            dispatch_input(&mut state, InputEvent::Submit),
            Action::Message("No available models found; keeping current model.".into())
        );
        assert!(!state.completion.visible);

        state.composer = "/".into();
        assert_eq!(
            dispatch_input(&mut state, InputEvent::Character('h')),
            Action::None
        );
        assert_eq!(state.completion.candidates, vec!["/help".to_string()]);
        assert_eq!(
            dispatch_input(&mut state, InputEvent::CancelOrQuit),
            Action::None
        );
        assert!(!state.completion.visible);
    }

    #[test]
    fn model_completion_filters_and_accepts_without_submitting() {
        let models = vec![
            "gpt-4o".to_string(),
            "antigravity/gemini".to_string(),
            "gpt-5".to_string(),
        ];
        let mut state = AppState::test_state();
        state.composer = "/model".into();

        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Submit, &models),
            Action::None
        );
        assert_eq!(state.completion.mode, Some(CompletionMode::Model));
        assert_eq!(state.completion.candidates, models);

        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Character(' '), &models),
            Action::None
        );
        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Character('g'), &models),
            Action::None
        );
        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Character('p'), &models),
            Action::None
        );
        assert_eq!(
            state.completion.candidates,
            vec!["gpt-4o".to_string(), "gpt-5".to_string()]
        );

        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Next, &models),
            Action::None
        );
        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Submit, &models),
            Action::None
        );
        assert_eq!(state.composer, "/model gpt-5");
        assert!(!state.completion.visible);
    }

    #[test]
    fn normal_prompt_and_running_behavior_stay_unchanged() {
        let mut state = AppState::test_state();
        state.composer = "hello".into();
        assert_eq!(
            dispatch_input(&mut state, InputEvent::Submit),
            Action::Submit("hello".into())
        );

        state.begin_generation();
        assert_eq!(
            dispatch_input(&mut state, InputEvent::Character('x')),
            Action::None
        );
        assert_eq!(state.composer, "hello");
        assert_eq!(
            dispatch_input(&mut state, InputEvent::CancelOrQuit),
            Action::Cancel
        );
    }
}
