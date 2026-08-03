use crate::{
    commands::{
        execute_command, filter_command_labels, filter_model_labels, parse_command, CommandContext,
        CommandResult,
    },
    input::{self, InputEvent},
    login::{spawn_provider_login, LoginEvent, LoginMode, LoginProvider},
    resolve_credentials, tui,
    ui::{reduce_agent_event, AppState, CompletionMode, MessageType, RunStatus, TranscriptMessage},
};
use crossterm::event;
use std::{path::PathBuf, sync::Arc, time::Duration};
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{CodingAgent, CodingAgentCancellation, CodingAgentOptions};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(PartialEq, Eq)]
pub(crate) enum Action {
    Submit(String),
    Cancel,
    Quit,
    Message(String),
    OpenLogin,
    StartLogin(LoginProvider),
    SetOpenAiKey(String),
    CancelLogin,
    None,
}

impl std::fmt::Debug for Action {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Submit(prompt) => formatter.debug_tuple("Submit").field(prompt).finish(),
            Self::Cancel => formatter.write_str("Cancel"),
            Self::Quit => formatter.write_str("Quit"),
            Self::Message(message) => formatter.debug_tuple("Message").field(message).finish(),
            Self::OpenLogin => formatter.write_str("OpenLogin"),
            Self::StartLogin(provider) => {
                formatter.debug_tuple("StartLogin").field(provider).finish()
            }
            Self::SetOpenAiKey(_) => formatter.write_str("SetOpenAiKey(<redacted>)"),
            Self::CancelLogin => formatter.write_str("CancelLogin"),
            Self::None => formatter.write_str("None"),
        }
    }
}

pub(crate) fn dispatch_input(state: &mut AppState, input: InputEvent) -> Action {
    dispatch_input_with_models(state, input, &[])
}

fn needs_model_catalog(state: &AppState, input: &InputEvent) -> bool {
    if state.login.is_some() {
        return false;
    }
    if matches!(state.status, RunStatus::Running) {
        return false;
    }
    matches!(state.completion.mode, Some(CompletionMode::Model))
        || matches!(
            input,
            InputEvent::Submit
                if state.composer.trim() == "/model"
                    || (state.completion.mode == Some(CompletionMode::Command)
                        && state
                            .completion
                            .candidates
                            .get(state.completion.selected)
                            .is_some_and(|candidate| candidate == "/model"))
        )
}

fn show_model_completion(state: &mut AppState, models: &[String]) -> Action {
    let query = state
        .composer
        .strip_prefix("/model")
        .unwrap_or_default()
        .trim();
    let candidates = filter_model_labels(query, models);
    if candidates.is_empty() {
        state.completion.visible = true;
        state.completion.candidates = candidates;
        state.completion.selected = 0;
        state.completion.mode = Some(CompletionMode::Model);
    } else {
        state.show_completion(CompletionMode::Model, candidates);
    }
    Action::None
}

fn refresh_completion(state: &mut AppState, models: &[String]) -> Action {
    if state.completion.mode == Some(CompletionMode::Model) || state.composer.starts_with("/model ")
    {
        return show_model_completion(state, models);
    }
    if state.composer == "/" || state.composer.starts_with('/') && !state.composer.contains(' ') {
        state.show_completion(
            CompletionMode::Command,
            filter_command_labels(&state.composer),
        );
        return Action::None;
    }
    if state.composer.trim() == "/model" {
        return show_model_completion(state, models);
    }
    state.close_completion();
    Action::None
}

fn accept_completion(state: &mut AppState, models: &[String], open_model_picker: bool) -> Action {
    let Some(candidate) = state
        .completion
        .candidates
        .get(state.completion.selected)
        .cloned()
    else {
        state.close_completion();
        return Action::None;
    };
    if open_model_picker
        && state.completion.mode == Some(CompletionMode::Command)
        && candidate == "/model"
    {
        state.composer = candidate;
        return show_model_completion(state, models);
    }
    state.composer = match state.completion.mode {
        Some(CompletionMode::Model) => format!("/model {candidate}"),
        _ => candidate,
    };
    state.close_completion();
    Action::None
}

fn append_text(target: &mut String, text: &str) {
    target.push_str(text);
}

fn dispatch_login_input(state: &mut AppState, input: InputEvent) -> Action {
    let Some(login) = state.login.as_mut() else {
        return Action::None;
    };

    if login.pending {
        if matches!(input, InputEvent::CancelOrQuit) {
            return Action::CancelLogin;
        }
        return Action::None;
    }

    match login.mode {
        LoginMode::ProviderPicker => match input {
            InputEvent::Submit => match login.selected_provider() {
                LoginProvider::OpenAi => {
                    login.select_provider(LoginProvider::OpenAi);
                    Action::None
                }
                provider => Action::StartLogin(provider),
            },
            InputEvent::Previous => {
                login.select_previous_provider();
                Action::None
            }
            InputEvent::Next => {
                login.select_next_provider();
                Action::None
            }
            InputEvent::CancelOrQuit => {
                state.close_login();
                Action::None
            }
            InputEvent::Resize
            | InputEvent::Tab
            | InputEvent::Backspace
            | InputEvent::Character(_)
            | InputEvent::Paste(_) => Action::None,
        },
        LoginMode::OpenAiKey => match input {
            InputEvent::Submit => match login.save_openai_key() {
                Ok(key) => {
                    state.close_login();
                    Action::SetOpenAiKey(key)
                }
                Err(message) => Action::Message(message),
            },
            InputEvent::CancelOrQuit => {
                state.close_login();
                Action::None
            }
            InputEvent::Backspace => {
                login.backspace_key();
                Action::None
            }
            InputEvent::Character(character) => {
                login.push_char(character);
                Action::None
            }
            InputEvent::Paste(text) => {
                login.push_paste(&text);
                Action::None
            }
            InputEvent::Previous | InputEvent::Next | InputEvent::Resize | InputEvent::Tab => {
                Action::None
            }
        },
    }
}

pub(crate) fn dispatch_input_with_models(
    state: &mut AppState,
    input: InputEvent,
    models: &[String],
) -> Action {
    if state.login.is_some() {
        return dispatch_login_input(state, input);
    }

    if state.completion.visible {
        match input {
            InputEvent::Submit => {
                return accept_completion(state, models, true);
            }
            InputEvent::Tab => {
                return accept_completion(state, models, false);
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
            InputEvent::Paste(text) if !matches!(state.status, RunStatus::Running) => {
                append_text(&mut state.composer, &text);
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
        InputEvent::Submit if state.composer.trim() == "/login" => {
            state.open_login();
            Action::OpenLogin
        }
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
        InputEvent::Paste(text) if !matches!(state.status, RunStatus::Running) => {
            append_text(&mut state.composer, &text);
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
        | InputEvent::Paste(_)
        | InputEvent::Backspace => Action::None,
    }
}

fn push_assistant_message(state: &mut AppState, content: String) {
    state.messages.push(TranscriptMessage {
        msg_type: MessageType::Assistant,
        content,
    });
}

fn cancel_login_task(state: &mut AppState, active_login: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = active_login.take() {
        handle.abort();
    }
    state.close_login();
}

async fn drain_login_events(
    state: &mut AppState,
    agent: &Arc<Mutex<CodingAgent>>,
    login_rx: &mut UnboundedReceiver<LoginEvent>,
) {
    while let Ok(event) = login_rx.try_recv() {
        handle_login_event(state, Some(&mut *agent.lock().await), event);
    }
}

async fn shutdown_login_flow(
    state: &mut AppState,
    agent: &Arc<Mutex<CodingAgent>>,
    active_login: &mut Option<JoinHandle<()>>,
    login_rx: &mut UnboundedReceiver<LoginEvent>,
) {
    drain_login_events(state, agent, login_rx).await;

    if let Some(handle) = active_login.take() {
        let had_pending_login = state.login.as_ref().is_some_and(|login| login.pending);
        if !handle.is_finished() {
            handle.abort();
            if had_pending_login {
                state.close_login();
            }
        }
        let _ = handle.await;
    }

    drain_login_events(state, agent, login_rx).await;
}

fn handle_login_event(state: &mut AppState, agent: Option<&mut CodingAgent>, event: LoginEvent) {
    let attempt_id = match &event {
        LoginEvent::DeviceCodePrompt { attempt_id, .. }
        | LoginEvent::BrowserPrompt { attempt_id, .. }
        | LoginEvent::CodexTokens { attempt_id, .. }
        | LoginEvent::AntigravityCredentials { attempt_id, .. }
        | LoginEvent::Failed { attempt_id, .. } => *attempt_id,
    };

    if state.login.as_ref().map(|login| login.attempt_id) != Some(attempt_id) {
        return;
    }

    match event {
        LoginEvent::DeviceCodePrompt { user_code, url, .. } => {
            if let Some(login) = state.login.as_mut() {
                login.set_status("Waiting for ChatGPT device approval...");
            }
            push_assistant_message(
                state,
                format!("Open {url} and enter code {user_code} to finish Codex login."),
            );
        }
        LoginEvent::BrowserPrompt { url, .. } => {
            if let Some(login) = state.login.as_mut() {
                login.set_status("Waiting for Google OAuth callback...");
            }
            push_assistant_message(
                state,
                format!("Open this URL in your browser to finish Antigravity login:\n{url}"),
            );
        }
        LoginEvent::CodexTokens { tokens, .. } => {
            let message = match threadlane_auth::save_credentials(&tokens) {
                Ok(()) => {
                    if let Some(agent) = agent {
                        agent.set_credentials(
                            tokens.access_token.clone(),
                            tokens.account_id.clone(),
                        );
                    }
                    "Codex login complete.".to_string()
                }
                Err(error) => error,
            };
            state.close_login();
            push_assistant_message(state, message);
        }
        LoginEvent::AntigravityCredentials { credentials, .. } => {
            let account = credentials
                .account_email
                .clone()
                .unwrap_or_else(|| "the active account".to_string());
            let message = match threadlane_auth::save_antigravity_credentials(&credentials) {
                Ok(()) => format!("Antigravity login complete for {account}."),
                Err(error) => error,
            };
            state.close_login();
            push_assistant_message(state, message);
        }
        LoginEvent::Failed { message, .. } => {
            state.close_login();
            push_assistant_message(state, message);
        }
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
    let (login_tx, mut login_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut next_login_attempt = 0_u64;
    let mut active_login: Option<JoinHandle<()>> = None;

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
                                    push_assistant_message(&mut state, content)
                                }
                                CommandResult::OpenLogin => state.open_login(),
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
                    Action::OpenLogin => state.open_login(),
                    Action::StartLogin(provider) => {
                        cancel_login_task(&mut state, &mut active_login);
                        next_login_attempt = next_login_attempt.wrapping_add(1);
                        let attempt_id = next_login_attempt;
                        if let Some(login) = state.login.as_mut() {
                            login.begin_provider_flow(
                                provider,
                                attempt_id,
                                format!("Starting {} login...", provider.label()),
                            );
                        } else {
                            state.open_login();
                            if let Some(login) = state.login.as_mut() {
                                login.begin_provider_flow(
                                    provider,
                                    attempt_id,
                                    format!("Starting {} login...", provider.label()),
                                );
                            }
                        }
                        active_login =
                            Some(spawn_provider_login(provider, attempt_id, login_tx.clone()));
                    }
                    Action::CancelLogin => cancel_login_task(&mut state, &mut active_login),
                    Action::SetOpenAiKey(api_key) => {
                        agent.lock().await.set_credentials(api_key, None);
                        push_assistant_message(&mut state, "OpenAI API key saved.".into());
                    }
                    Action::Quit => break,
                    Action::Message(content) => push_assistant_message(&mut state, content),
                    Action::Submit(_) | Action::None => {}
                }
            }
        }

        drain_login_events(&mut state, &agent, &mut login_rx).await;
        while let Ok(event) = event_rx.try_recv() {
            reduce_agent_event(&mut state, event);
        }
    }

    if matches!(state.status, RunStatus::Running) {
        if let Err(error) = cancellation.cancel() {
            reduce_agent_event(&mut state, AgentEvent::AgentError { error });
        }
    }
    shutdown_login_flow(&mut state, &agent, &mut active_login, &mut login_rx).await;
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

        assert_eq!(dispatch_input(&mut state, InputEvent::Submit), Action::None);
        assert!(state.completion.visible);
        assert_eq!(state.completion.mode, Some(CompletionMode::Model));
        assert!(state.completion.candidates.is_empty());
        assert_eq!(
            dispatch_input(&mut state, InputEvent::CancelOrQuit),
            Action::None
        );

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
    fn empty_model_filter_stays_open_and_escape_cancels_it() {
        let models = vec!["gpt-4o".to_string()];
        let mut state = AppState::test_state();
        state.composer = "/model".into();

        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Submit, &models),
            Action::None
        );
        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Character('z'), &models),
            Action::None
        );
        assert!(state.completion.visible);
        assert_eq!(state.completion.mode, Some(CompletionMode::Model));
        assert!(state.completion.candidates.is_empty());

        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Character('z'), &models),
            Action::None
        );
        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::CancelOrQuit, &models),
            Action::None
        );
        assert!(!state.completion.visible);
    }

    #[test]
    fn typing_exact_model_command_does_not_request_model_catalog() {
        let mut state = AppState::test_state();
        state.composer = "/mode".into();
        state.show_completion(CompletionMode::Command, vec!["/model".into()]);

        assert!(!needs_model_catalog(&state, &InputEvent::Character('l')));

        state.composer.push('l');
        assert!(needs_model_catalog(&state, &InputEvent::Submit));
    }

    #[test]
    fn enter_on_model_command_completion_opens_model_picker() {
        let models = vec!["gpt-4o".to_string(), "gpt-5".to_string()];
        let mut state = AppState::test_state();

        for character in "/model".chars() {
            assert_eq!(
                dispatch_input_with_models(&mut state, InputEvent::Character(character), &models),
                Action::None
            );
        }
        assert_eq!(state.completion.mode, Some(CompletionMode::Command));

        assert_eq!(
            dispatch_input_with_models(&mut state, InputEvent::Submit, &models),
            Action::None
        );

        assert_eq!(state.composer, "/model");
        assert!(state.completion.visible);
        assert_eq!(state.completion.mode, Some(CompletionMode::Model));
        assert_eq!(state.completion.candidates, models);
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

    #[test]
    fn login_command_opens_provider_picker_and_blocks_prompt_submission() {
        let mut state = AppState::test_state();
        state.composer = "/login".into();

        assert_eq!(
            dispatch_input(&mut state, InputEvent::Submit),
            Action::OpenLogin
        );
        assert_eq!(
            state.login.as_ref().unwrap().mode,
            LoginMode::ProviderPicker
        );

        state.composer = "should stay blocked".into();
        assert_eq!(
            dispatch_input(&mut state, InputEvent::Character('!')),
            Action::None
        );
        assert_eq!(state.composer, "should stay blocked");
    }

    #[test]
    fn login_provider_picker_selects_openai_and_escape_cancels() {
        let mut state = AppState::test_state();
        state.open_login();

        assert_eq!(
            dispatch_input(&mut state, InputEvent::Submit),
            Action::StartLogin(LoginProvider::Codex)
        );

        state.open_login();
        assert_eq!(dispatch_input(&mut state, InputEvent::Next), Action::None);
        assert_eq!(dispatch_input(&mut state, InputEvent::Submit), Action::None);
        assert_eq!(state.login.as_ref().unwrap().mode, LoginMode::OpenAiKey);

        assert_eq!(
            dispatch_input(&mut state, InputEvent::CancelOrQuit),
            Action::None
        );
        assert!(state.login.is_none());
    }

    #[test]
    fn login_openai_key_rejects_empty_submit_and_accepts_paste() {
        struct HomeGuard {
            _lock: std::sync::MutexGuard<'static, ()>,
            previous_home: Option<std::ffi::OsString>,
            home: std::path::PathBuf,
        }

        impl HomeGuard {
            fn new() -> Self {
                let lock = crate::test_env_guard_lock();
                let previous_home = std::env::var_os("HOME");
                let home = std::env::temp_dir().join(format!(
                    "threadlane-cli-openai-login-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::create_dir_all(&home).unwrap();
                std::env::set_var("HOME", &home);
                Self {
                    _lock: lock,
                    previous_home,
                    home,
                }
            }
        }

        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match self.previous_home.take() {
                    Some(home) => std::env::set_var("HOME", home),
                    None => std::env::remove_var("HOME"),
                }
                let _ = std::fs::remove_dir_all(&self.home);
            }
        }

        let _env = HomeGuard::new();
        let mut state = AppState::test_state();
        state.open_login();
        assert_eq!(dispatch_input(&mut state, InputEvent::Next), Action::None);
        assert_eq!(dispatch_input(&mut state, InputEvent::Submit), Action::None);

        assert_eq!(
            dispatch_input(&mut state, InputEvent::Submit),
            Action::Message("OpenAI API key cannot be empty.".into())
        );
        assert_eq!(
            dispatch_input(&mut state, InputEvent::Paste("sk-secret".into())),
            Action::None
        );
        assert_eq!(state.login.as_ref().unwrap().masked_key(), "*********");
        assert!(matches!(
            dispatch_input(&mut state, InputEvent::Submit),
            Action::SetOpenAiKey(key) if key == "sk-secret"
        ));
    }

    #[test]
    fn paste_appends_to_normal_composer_when_login_is_closed() {
        let mut state = AppState::test_state();

        assert_eq!(
            dispatch_input(&mut state, InputEvent::Paste("hello".into())),
            Action::None
        );
        assert_eq!(state.composer, "hello");
    }

    #[tokio::test]
    async fn cancelling_pending_login_aborts_task_and_ignores_stale_completion() {
        use std::fs;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct HomeGuard {
            _lock: std::sync::MutexGuard<'static, ()>,
            saved_home: Option<std::ffi::OsString>,
            home: PathBuf,
        }

        impl HomeGuard {
            fn new() -> Self {
                let lock = crate::test_env_guard_lock();
                let saved_home = std::env::var_os("HOME");
                let home = std::env::temp_dir().join(format!(
                    "threadlane-cli-login-cancel-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                fs::create_dir_all(&home).unwrap();
                std::env::set_var("HOME", &home);
                Self {
                    _lock: lock,
                    saved_home,
                    home,
                }
            }
        }

        impl Drop for HomeGuard {
            fn drop(&mut self) {
                if let Some(saved_home) = self.saved_home.take() {
                    std::env::set_var("HOME", saved_home);
                } else {
                    std::env::remove_var("HOME");
                }
                let _ = fs::remove_dir_all(&self.home);
            }
        }

        let env = HomeGuard::new();
        let mut state = AppState::test_state();
        state.open_login();
        state.login.as_mut().unwrap().begin_provider_flow(
            LoginProvider::Codex,
            41,
            "Starting Codex login...",
        );

        let mut active_login = Some(tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }));
        let baseline_messages = state.messages.len();

        assert_eq!(
            dispatch_input(&mut state, InputEvent::CancelOrQuit),
            Action::CancelLogin
        );

        cancel_login_task(&mut state, &mut active_login);
        assert!(state.login.is_none());
        assert!(active_login.is_none());

        handle_login_event(
            &mut state,
            None,
            LoginEvent::CodexTokens {
                attempt_id: 41,
                tokens: Box::new(threadlane_auth::OAuthTokens {
                    access_token: "token-should-not-save".into(),
                    refresh_token: Some("refresh-should-not-save".into()),
                    expires_in: Some(60),
                    id_token: None,
                    account_id: Some("acct".into()),
                }),
            },
        );

        assert_eq!(state.messages.len(), baseline_messages);
        assert!(
            !env.home
                .join(".threadlane")
                .join("credentials.json")
                .exists(),
            "stale login completion must not persist credentials after cancellation"
        );
    }

    #[tokio::test]
    async fn shutdown_processes_queued_login_success_before_aborting_active_flow() {
        use std::fs;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct HomeGuard {
            _lock: std::sync::MutexGuard<'static, ()>,
            saved_home: Option<std::ffi::OsString>,
            home: PathBuf,
        }

        impl HomeGuard {
            fn new() -> Self {
                let lock = crate::test_env_guard_lock();
                let saved_home = std::env::var_os("HOME");
                let home = std::env::temp_dir().join(format!(
                    "threadlane-cli-login-shutdown-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                fs::create_dir_all(&home).unwrap();
                std::env::set_var("HOME", &home);
                Self {
                    _lock: lock,
                    saved_home,
                    home,
                }
            }
        }

        impl Drop for HomeGuard {
            fn drop(&mut self) {
                if let Some(saved_home) = self.saved_home.take() {
                    std::env::set_var("HOME", saved_home);
                } else {
                    std::env::remove_var("HOME");
                }
                let _ = fs::remove_dir_all(&self.home);
            }
        }

        let env = HomeGuard::new();
        let mut state = AppState::test_state();
        state.open_login();
        state.login.as_mut().unwrap().begin_provider_flow(
            LoginProvider::Codex,
            77,
            "Starting Codex login...",
        );

        let (login_tx, mut login_rx) = tokio::sync::mpsc::unbounded_channel();
        let success_tokens = threadlane_auth::OAuthTokens {
            access_token: "queued-success-token".into(),
            refresh_token: Some("queued-refresh-token".into()),
            expires_in: Some(60),
            id_token: None,
            account_id: Some("acct-queued".into()),
        };
        login_tx
            .send(LoginEvent::CodexTokens {
                attempt_id: 77,
                tokens: Box::new(success_tokens),
            })
            .unwrap();

        let mut active_login = Some(tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }));

        let agent = Arc::new(Mutex::new(CodingAgent::new(CodingAgentOptions {
            api_key: "before-login".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: env.home.clone(),
            session_file: None,
            system_prompt: Default::default(),
        })));

        shutdown_login_flow(&mut state, &agent, &mut active_login, &mut login_rx).await;

        assert!(state.login.is_none());
        assert!(active_login.is_none());
        assert!(
            state
                .messages
                .iter()
                .any(|message| message.content == "Codex login complete."),
            "queued success should still be applied during shutdown"
        );
        let saved =
            fs::read_to_string(env.home.join(".threadlane").join("credentials.json")).unwrap();
        assert!(saved.contains("queued-success-token"));
        assert!(saved.contains("acct-queued"));
    }
}
