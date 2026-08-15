//! Async chat execution boundary for GPUI.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};

use threadlane_agent::{AgentEvent, ImageAttachment, SessionTree};
use threadlane_provider::ProviderClient;

use crate::services::sessions::SessionRuntime;
use crate::state::ChatStreamEvent;

pub(crate) fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    static EXECUTOR: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    EXECUTOR
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("threadlane-gpui-agent")
                .build()
                .map_err(|error| format!("Failed to start agent runtime: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

struct RunCleanup {
    runtime: Arc<SessionRuntime>,
    registration_id: u64,
    session_id: String,
    stream_tx: Sender<ChatStreamEvent>,
    error: Option<String>,
}

impl Drop for RunCleanup {
    fn drop(&mut self) {
        self.runtime
            .cancellation
            .finish_active_run(self.registration_id);
        self.runtime.finish_generation(self.error.clone());
        let _ = self.stream_tx.send(ChatStreamEvent::Finished {
            session_id: self.session_id.clone(),
            session_file: self.runtime.session_file.clone(),
        });
    }
}

pub fn execute_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    text: String,
    images: Vec<ImageAttachment>,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    runtime.begin_generation()?;
    let executor = match executor() {
        Ok(executor) => executor,
        Err(error) => {
            runtime.finish_generation(Some(error.clone()));
            return Err(error);
        }
    };

    let task_runtime = runtime.clone();
    let task_session_id = session_id.clone();
    let task_stream_tx = stream_tx.clone();
    let (registration_tx, registration_rx) = tokio::sync::oneshot::channel();
    let task = executor.spawn(async move {
        let Ok(registration_id) = registration_rx.await else {
            task_runtime.finish_generation(Some("Generation registration failed".into()));
            return;
        };

        let mut cleanup = RunCleanup {
            runtime: task_runtime.clone(),
            registration_id,
            session_id: task_session_id.clone(),
            stream_tx: task_stream_tx.clone(),
            error: None,
        };
        let mut agent = task_runtime.agent.lock().await;
        let mut events = agent.subscribe();
        let run_error = {
            let run = agent.handle_input_with_images(&text, images);
            tokio::pin!(run);
            let mut run_error = None;
            let mut saw_agent_error = false;

            loop {
                tokio::select! {
                    result = &mut run => {
                        match result {
                            Some(Ok(output)) if !output.is_empty() => {
                                let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                    session_id: task_session_id.clone(),
                                    event: AgentEvent::MessageUpdate {
                                        text_delta: Some(output),
                                        reasoning_delta: None,
                                        tool_call_name: None,
                                    },
                                });
                            }
                            Some(Err(error)) => {
                                run_error = Some(error);
                            }
                            _ => {}
                        }
                        while let Ok(event) = events.try_recv() {
                            saw_agent_error |= matches!(event, AgentEvent::AgentError { .. });
                            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                session_id: task_session_id.clone(),
                                event,
                            });
                        }
                        if let Some(error) = run_error.as_ref().filter(|_| !saw_agent_error) {
                            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                session_id: task_session_id.clone(),
                                event: AgentEvent::AgentError { error: error.clone() },
                            });
                        }
                        break;
                    }
                    event = events.recv() => {
                        match event {
                            Ok(event) => {
                                saw_agent_error |= matches!(event, AgentEvent::AgentError { .. });
                                let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                    session_id: task_session_id.clone(),
                                    event,
                                });
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
            run_error
        };

        drop(agent);
        cleanup.error = run_error;
    });

    let registration_id = match runtime.cancellation.track_active_run(task.abort_handle()) {
        Ok(id) => id,
        Err(error) => {
            task.abort();
            runtime.finish_generation(Some(error.clone()));
            return Err(error);
        }
    };
    if registration_tx.send(registration_id).is_err() {
        runtime.cancellation.finish_active_run(registration_id);
        let error = "Generation task stopped before registration".to_string();
        runtime.finish_generation(Some(error.clone()));
        return Err(error);
    }
    Ok(())
}

pub fn spawn_session_title(
    session_file: PathBuf,
    session_id: String,
    submitted_prompt: String,
    api_key: String,
    account_id: Option<String>,
    model: String,
    stream_tx: Sender<ChatStreamEvent>,
) {
    let mut tree = match SessionTree::load_from_file(&session_file) {
        Ok(tree) => tree,
        Err(error) => {
            log::warn!(
                "unable to load session {} for automatic title generation ({}): {}",
                session_id,
                session_file.display(),
                error
            );
            return;
        }
    };
    if tree.has_name() || submitted_prompt.trim().is_empty() {
        return;
    }
    match tree.mark_title_attempted() {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            log::warn!(
                "unable to persist automatic title attempt for session {}: {}",
                session_id,
                error
            );
            return;
        }
    }

    let Ok(executor) = executor() else {
        return;
    };
    executor.spawn(async move {
        let result = async {
            let client = ProviderClient::new(api_key, account_id);
            let raw = client.generate_title(&model, &submitted_prompt).await?;
            let title = normalize_session_title(&raw);
            if title.is_empty() {
                return Err("title normalization produced an empty title".to_string());
            }
            let mut tree = SessionTree::load_from_file(&session_file)
                .map_err(|error| format!("reload failed: {error}"))?;
            if tree.has_name() {
                return Err("session was named while title generation was running".to_string());
            }
            tree.set_name(title)
                .map_err(|error| format!("persistence failed: {error}"))
        }
        .await;

        if let Err(error) = result {
            log::warn!(
                "automatic title generation failed for session {}: {}",
                session_id,
                error
            );
            return;
        }
        let _ = stream_tx.send(ChatStreamEvent::TitleGenerated {
            session_id,
            session_file,
        });
    });
}

fn normalize_session_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    loop {
        let before = title.clone();
        if title
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("title:"))
        {
            title = title[6..].trim().to_string();
        }
        let quoted = ((title.starts_with('"') && title.ends_with('"'))
            || (title.starts_with('\'') && title.ends_with('\'')))
            && title.len() >= 2;
        if quoted {
            title = title[1..title.len() - 1].trim().to_string();
        }
        if title == before {
            break;
        }
    }

    let mut collapsed = String::with_capacity(title.len());
    let mut previous_was_space = true;
    for character in title.chars() {
        if character.is_whitespace() {
            if !previous_was_space {
                collapsed.push(' ');
                previous_was_space = true;
            }
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }
    if collapsed.ends_with(' ') {
        collapsed.pop();
    }
    collapsed.chars().take(42).collect()
}

pub fn cancel_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    runtime.cancel()?;
    runtime.finish_generation(Some("Generation cancelled".into()));
    let _ = stream_tx.send(ChatStreamEvent::Agent {
        session_id: session_id.clone(),
        event: AgentEvent::AgentError {
            error: "Generation cancelled".into(),
        },
    });
    let _ = stream_tx.send(ChatStreamEvent::Finished {
        session_id,
        session_file: runtime.session_file.clone(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize_session_title;

    #[test]
    fn title_normalization_matches_native_behavior() {
        assert_eq!(
            normalize_session_title("  \"Title:   Wire automatic titles  \" "),
            "Wire automatic titles"
        );
        assert_eq!(
            normalize_session_title(
                "A title that is deliberately much longer than forty-two characters"
            ),
            "A title that is deliberately much longer t"
        );
    }
}
