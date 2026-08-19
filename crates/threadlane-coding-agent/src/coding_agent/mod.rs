pub mod broker;
pub mod capabilities;
pub mod harness;
pub use capabilities::*;
pub mod runtime;
// Re-export public runtime items (explicit list to avoid conflicts with
// harness_journal module).
pub(crate) use runtime::{
    abort_open_subagent_operations, AgentRunner, AgentWork, AgentWorkScheduler, MAX_SUBAGENT_TASKS,
    MAX_SUBAGENT_TASK_CHARS,
};
pub use runtime::{
    cancel_open_subagent_operations, AgentRunTask, CodingAgent, CodingAgentCancellation,
    CodingAgentOptions, CodingAgentWorkHandle, HarnessCompositionSnapshot,
    SubagentCancellationGuard, SubagentInnerTool, SubagentInnerToolData, SubagentResult,
    SubagentSessionData,
};
#[cfg(test)]
pub(crate) use runtime::{
    generation_event_drain_error, is_retryable_generation_error, recover_v2_subagent_records,
    subagent_ui_event,
};
// Re-export test-only types.
pub(crate) use broker::*;
pub use harness::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_broker::{
        BrokerError, BrokerRequest, CapabilityDispatcher, CapabilityHandler, BROKER_API_VERSION,
    };
    use crate::policy::ToolPolicy;
    use crate::system_prompt::SystemPromptConfig;
    use serde_json::Value;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration as StdDuration, Instant};
    use threadlane_agent::harness::{
        AgentHarness, HookContext, HookEffect, HookHandler, HookKind, JsonlStore, OperationIntent,
        OperationOutcome, QueueKind, Record, Reducer, SessionStore,
        ToolResult as HarnessToolResult,
    };
    use threadlane_wasi::{
        WasiExtension, WasiExtensionInvocation, WasiExtensionManager, WasiExtensionResponse,
    };
    // Alias for test compatibility — production code uses Record, but many
    // tests were written against the HarnessRecord name previously in scope.
    type HarnessRecord = Record;
    use threadlane_agent::{
        AgentEvent, AgentMessage, ImageAttachment, SessionTree, TokenUsage, ToolExecutor,
    };
    use tokio::sync::broadcast;
    use tokio::time::Duration;

    #[test]
    fn lagged_generation_event_drain_is_recoverable() {
        assert_eq!(
            generation_event_drain_error(broadcast::error::TryRecvError::Lagged(3)),
            None
        );
        assert_eq!(
            generation_event_drain_error(broadcast::error::TryRecvError::Empty),
            Some("generation ended without a durable AgentEnd event")
        );
    }

    #[test]
    fn harness_journal_round_trips_foreground_operation_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        journal
            .finish("run-1", OperationOutcome::Completed, None)
            .unwrap();
        let reopened = HarnessJournal::open(&path).unwrap();
        assert_eq!(reopened.store.records().len(), 2);
        assert!(reopened
            .store
            .records()
            .windows(2)
            .all(|pair| pair[0].seq() < pair[1].seq()));
    }

    #[tokio::test]
    async fn failed_run_persists_an_error_message_for_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut agent = CodingAgent::new(options);
        let run_id = agent
            .begin_harness_run(AgentMessage::user("prompt", vec![]))
            .await
            .unwrap()
            .unwrap();

        agent
            .finish_harness_run(
                Some(&run_id.run_id),
                OperationOutcome::Failed,
                Some("provider failed".into()),
            )
            .await
            .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.entries().iter().any(|entry| {
            matches!(
                &entry.message,
                AgentMessage::Custom { custom_type, payload }
                    if custom_type == "agent_error"
                        && payload.get("error").and_then(Value::as_str)
                            == Some("provider failed")
            )
        }));
    }

    #[tokio::test]
    async fn accepted_harness_prompt_uses_its_canonical_entry_as_active_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut agent = CodingAgent::new(options);

        let accepted = agent
            .begin_harness_run(AgentMessage::user("prompt", vec![]))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(accepted.lane, "main");
        assert!(!accepted.run_id.is_empty());
        assert!(!accepted.prompt_entry_id.is_empty());
        assert!(accepted.accepted_through_seq > 0);
        let active_leaf = agent.session_tree.active_node_id().unwrap();
        let store = JsonlStore::open(&path).unwrap();
        let prompt_entry = store
            .entries()
            .iter()
            .find(|entry| entry.id == accepted.prompt_entry_id)
            .expect("accepted prompt entry must be durable");
        assert!(matches!(
            &prompt_entry.message,
            AgentMessage::User { content } if content == "prompt"
        ));
        assert!(prompt_entry.seq <= accepted.accepted_through_seq);
        let active_entry = store
            .entries()
            .iter()
            .find(|entry| entry.id == active_leaf)
            .expect("active assistant target must be durable");
        assert!(active_entry.seq <= accepted.accepted_through_seq);
        assert_eq!(
            store
                .entries()
                .iter()
                .filter(|entry| matches!(entry.message, AgentMessage::User { .. }))
                .count(),
            1
        );
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationStarted { .. }))
                .count(),
            1
        );
        assert!(!active_leaf.starts_with("node_"));
        let context = store
            .records()
            .iter()
            .find(|record| matches!(record, HarnessRecord::RunContextCaptured { .. }))
            .expect("accepted run must persist its resolved context");
        assert!(matches!(
            context,
            HarnessRecord::RunContextCaptured {
                system_prompt: threadlane_agent::harness::PromptSnapshot::Full { content, .. },
                model,
                provider,
                enabled_tool_names,
                ..
            } if !content.is_empty()
                && !model.as_str().is_empty()
                && !provider.as_str().is_empty()
                && !enabled_tool_names.is_empty()
        ));
    }

    #[tokio::test]
    async fn second_prompt_acceptance_is_rejected_while_run_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path);
        let mut agent = CodingAgent::new(options);
        let accepted = agent
            .begin_harness_run(AgentMessage::user("first", vec![]))
            .await
            .unwrap()
            .unwrap();

        let error = agent
            .begin_harness_run(AgentMessage::user("second", vec![]))
            .await
            .unwrap_err();
        assert!(error.contains(&accepted.run_id));
        assert!(error.contains("cannot be repeated"));
    }

    #[test]
    fn adopted_background_run_captures_context_before_provider_work() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut agent = CodingAgent::new(options);
        let mut harness = HarnessJournal::open(&path).unwrap();
        let accepted = harness
            .begin_run("background-run", AgentMessage::user("prompt", vec![]))
            .unwrap();

        agent.adopt_harness_run(&accepted).unwrap();

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::RunContextCaptured { run_id, .. } if run_id == "background-run"
        )));
    }

    #[test]
    fn v2_only_subagent_records_are_recoverable_without_the_legacy_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::File::create(&path).unwrap();
        let mut harness = AgentHarness::new(JsonlStore::open(&path).unwrap());
        harness
            .start_operation_on_lane("subagent-lane", "child-run", None, OperationIntent::Run)
            .unwrap();
        harness.drive_one().unwrap();

        let records = recover_v2_subagent_records(&path).unwrap();
        assert!(records.iter().any(|record| {
            matches!(record, HarnessRecord::OperationStarted { id, lane, .. } if id == "child-run" && lane == "subagent-lane")
        }));
        assert!(JsonlStore::open(&path)
            .unwrap()
            .records()
            .iter()
            .any(|record| record.id() == "child-run"));
    }

    #[test]
    fn v2_recovery_ignores_foreground_operations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .begin_run("foreground", AgentMessage::user("hello", vec![]))
            .unwrap();

        assert!(recover_v2_subagent_records(&path).unwrap().is_empty());
    }

    #[test]
    fn idle_saved_sessions_start_with_recovery_complete() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);

        let agent = CodingAgent::new(options);

        assert!(matches!(
            agent.interrupted_subagent_recovery,
            InterruptedSubagentRecoveryState::Complete
        ));
    }
    #[test]
    fn interrupted_subagent_sessions_report_has_interrupted_work() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();
        drop(journal);

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);

        let agent = CodingAgent::new(options);

        assert!(agent.has_interrupted_work());
    }

    #[test]
    fn harness_journal_reuses_the_provisioned_assistant_result_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .begin_run("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: Some("world".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();
        let attempts: Vec<_> = journal
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::StepAttempt {
                    run_id,
                    result_entry_id,
                    ..
                } if run_id == "run-1" => Some(result_entry_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec!["entry-run-1-assistant-1"]);
        assert!(journal.store.entries().iter().any(|entry| {
            entry.id == "entry-run-1-assistant-1"
                && matches!(entry.message, AgentMessage::Assistant { .. })
        }));
    }

    #[test]
    fn harness_journal_attaches_the_next_turn_to_its_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();

        journal
            .begin_run("run-1", AgentMessage::user("first", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: Some("one".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();
        journal
            .finish("run-1", OperationOutcome::Completed, None)
            .unwrap();

        journal
            .begin_run("run-2", AgentMessage::user("second", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: Some("two".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();

        let prompt_id = "entry-run-2-user";
        assert_eq!(
            journal
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == "entry-run-2-assistant-1")
                .and_then(|entry| entry.parent_id.as_deref()),
            Some(prompt_id)
        );
    }

    #[test]
    fn harness_journal_commits_assistant_intent_before_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .begin_run("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();

        let result_id = journal.prepare_assistant_attempt("run-1").unwrap();
        assert_eq!(result_id, "entry-run-1-assistant-1");
        assert!(journal
            .store
            .entries()
            .iter()
            .all(|entry| { !matches!(entry.message, AgentMessage::Assistant { .. }) }));
        assert!(journal.store.records().iter().any(|record| {
            matches!(
                record,
                HarnessRecord::StepAttempt {
                    run_id,
                    attempt: 1,
                    result_entry_id,
                    ..
                } if run_id == "run-1" && result_entry_id == &result_id
            )
        }));

        journal
            .append_message(AgentMessage::Assistant {
                content: Some("world".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();
        assert!(journal
            .store
            .entries()
            .iter()
            .any(|entry| entry.id == result_id));
    }

    #[tokio::test]
    async fn harness_journal_closes_a_tool_at_result_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .begin_run("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "README.md"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();
        let result = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "contents".into(),
            is_error: false,
            terminate: false,
        };
        journal.append_message(result.clone()).unwrap();
        journal.finish_tool_message("run-1", &result).unwrap();

        assert!(journal.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::ToolFinished {
                run_id,
                tool_call_id,
                ..
            } if run_id == "run-1" && tool_call_id == "call-1"
        )));
    }

    #[tokio::test]
    async fn duplicate_tool_intent_does_not_rerun_before_tool_hook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        let hook_calls = Arc::new(AtomicU64::new(0));
        let hook_calls_for_handler = hook_calls.clone();
        journal
            .store
            .hooks_mut()
            .register(
                HookKind::BeforeTool,
                "count-before-tool",
                Arc::new(move |_| {
                    hook_calls_for_handler.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(HookEffect::default()) })
                }),
            )
            .unwrap();
        journal
            .begin_run("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "README.md"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();

        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn foreground_resume_replays_safe_tool_through_harness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .begin_run("run-1", AgentMessage::user("inspect", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "session.jsonl"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "session.jsonl"}),
            )
            .await
            .unwrap();
        drop(journal);

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut coding_agent = CodingAgent::new(options);
        assert!(coding_agent
            .recover_harness_tool_batch("run-1")
            .await
            .unwrap());

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::Usage {
                cause: threadlane_agent::harness::UsageCause::Replay,
                tool_call_id: Some(tool_call_id),
                ..
            } if tool_call_id == "call-1"
        )));
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::ToolFinished {
                run_id,
                tool_call_id,
                ..
            } if run_id == "run-1" && tool_call_id == "call-1"
        )));
        assert!(store.entries().iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Tool { tool_call_id, .. } if tool_call_id == "call-1"
        )));
    }

    #[test]
    fn harness_run_ids_skip_persisted_ids_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("foreground-1", None).unwrap();
        journal
            .finish("foreground-1", OperationOutcome::Completed, None)
            .unwrap();

        let mut reopened = HarnessJournal::open(&path).unwrap();
        let next = reopened.unique_run_id("foreground").unwrap();
        assert_ne!(next, "foreground-1");
        reopened.start(&next, None).unwrap();
    }

    #[test]
    fn harness_retry_survives_restart_and_consumes_before_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        assert_eq!(journal.schedule_retry("run-1", "timeout").unwrap(), 1);
        drop(journal);

        let mut reopened = HarnessJournal::open(&path).unwrap();
        assert!(Reducer::reduce(&reopened.store)
            .unwrap()
            .lane("main")
            .unwrap()
            .retry
            .is_some());
        assert_eq!(reopened.begin_retry("run-1").unwrap(), 1);
        assert_eq!(
            Reducer::reduce(&reopened.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .attempts,
            1
        );
    }

    #[test]
    fn retryable_generation_error_markers_are_narrow() {
        assert!(is_retryable_generation_error("provider timeout"));
        assert!(is_retryable_generation_error("HTTP status 503"));
        assert!(!is_retryable_generation_error("invalid request"));
    }

    #[test]
    fn harness_journal_records_the_assistant_attempt_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        journal
            .store
            .append_entry(threadlane_agent::harness::Entry {
                id: "assistant-1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 3,
                timestamp: 3,
                message: AgentMessage::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_agent::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        journal
            .record_assistant_attempt(
                "run-1",
                TokenUsage {
                    output_tokens: 2,
                    total_tokens: 2,
                    ..TokenUsage::default()
                },
            )
            .unwrap();
        let reopened = HarnessJournal::open(&path).unwrap();
        assert!(reopened.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::StepAttempt { run_id, result_entry_id, .. }
                if run_id == "run-1" && result_entry_id == "assistant-1"
        )));
        assert!(reopened.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::Usage { run_id: Some(run_id), usage, .. }
                if run_id == "run-1" && usage.output_tokens == 2
        )));
    }

    #[test]
    fn harness_journal_records_a_completed_tool_batch_in_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-tools", None).unwrap();
        journal
            .store
            .append_entry(threadlane_agent::harness::Entry {
                id: "assistant-tools".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{\"path\":\"README.md\"}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_agent::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        journal
            .store
            .append_entry(threadlane_agent::harness::Entry {
                id: "tool-result-1".into(),
                parent_id: Some("assistant-tools".into()),
                lane: "main".into(),
                seq: 3,
                timestamp: 3,
                message: AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "contents".into(),
                    is_error: false,
                    terminate: true,
                },
                surface_op: threadlane_agent::harness::SurfaceOperation::Append,
                terminate: true,
            })
            .unwrap();
        journal
            .record_completed_tools_with_termination(
                "run-tools",
                &HashMap::from([(String::from("call-1"), true)]),
            )
            .unwrap();
        let reopened = HarnessJournal::open(&path).unwrap();
        let reduced = Reducer::reduce(&reopened.store).unwrap();
        assert!(reduced
            .lane("main")
            .unwrap()
            .tools
            .iter()
            .all(|tool| tool.completed));
        assert!(reduced
            .lane("main")
            .unwrap()
            .tools
            .iter()
            .any(|tool| tool.terminate));
    }

    #[test]
    fn harness_journal_abort_is_durable_and_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        assert_eq!(journal.request_abort().unwrap().as_deref(), Some("run-1"));
        assert!(journal.recover_abort().unwrap());
        let reopened = HarnessJournal::open(&path).unwrap();
        assert!(reopened
            .store
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::AbortRequested { .. })));
        let attempt_seq = reopened.store.records().iter().find_map(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id, .. } if run_id == "run-1")
                .then_some(record.seq())
        });
        let aborted_entry_seq = reopened.store.entries().iter().find_map(|entry| {
            matches!(
                &entry.message,
                AgentMessage::Assistant {
                    stop_reason: Some(reason),
                    ..
                } if reason == "aborted"
            )
            .then_some(entry.seq)
        });
        assert!(attempt_seq
            .is_some_and(|attempt| { aborted_entry_seq.is_some_and(|entry| attempt < entry) }));
        assert!(reopened.store.records().iter().any(
            |record| matches!(record, HarnessRecord::OperationFinished { run_id, outcome: OperationOutcome::Aborted, .. } if run_id == "run-1")
        ));
    }

    #[tokio::test]
    async fn suspended_harness_with_a_persisted_assistant_finishes_without_replaying_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut agent = CodingAgent::new(options);
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_record(HarnessRecord::OperationStarted {
                id: "run-resume".into(),
                seq: 1,
                lane: "main".into(),
                timestamp: 1,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_entry(threadlane_agent::harness::Entry {
                id: "assistant-resume".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: Some("already persisted".into()),
                    tool_calls: None,
                    stop_reason: Some("stop".into()),
                    deferred_handle: None,
                },
                surface_op: threadlane_agent::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();

        assert!(agent.resume_suspended_harness().await.unwrap());
        let reopened = JsonlStore::open(&path).unwrap();
        assert!(reopened.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished { run_id, outcome: OperationOutcome::Completed, .. }
                if run_id == "run-resume"
        )));
    }

    #[test]
    fn subagent_journal_allocates_durable_unique_run_ids_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut first = HarnessJournal::open(&session_file).unwrap();
        let first_run = first
            .start_subagent_lane("subagent-1:0", "inspect", Some("node_1"))
            .unwrap();
        first
            .finish_subagent_lane(
                &first_run.lane_name,
                &first_run.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();
        drop(first);

        let mut second = HarnessJournal::open(&session_file).unwrap();
        let second_run = second
            .start_subagent_lane("subagent-1:0", "inspect again", Some("node_1"))
            .unwrap();
        second
            .finish_subagent_lane(
                &second_run.lane_name,
                &second_run.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();

        assert_ne!(first_run.run_id, second_run.run_id);
        assert_ne!(first_run.lane_name, second_run.lane_name);
        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn subagent_journal_writes_v2_run_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        let leaf = tree.add_message(AgentMessage::User {
            content: "parent task".into(),
        });
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let run = journal
            .start_subagent_lane("subagent-1:0", "inspect", Some(&leaf))
            .unwrap();
        journal
            .finish_subagent_lane(
                &run.lane_name,
                &run.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationStarted { id, lane, .. }
                if id == &run.run_id && lane == &run.lane_name
        )));
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            } if run_id == &run.run_id
        )));
        assert!(store.entries().iter().any(|entry| {
            entry.lane == run.lane_name
                && matches!(&entry.message, AgentMessage::User { content } if content == "inspect")
        }));
        assert!(Reducer::reduce(&store).is_ok());
    }

    #[test]
    fn subagent_journal_reuses_v2_assistant_result_id() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::write(&session_file, "").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let run = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        let assistant = AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("stop".into()),
            deferred_handle: None,
        };

        let entry_id = journal
            .append_message_to_lane(&run.lane_name, &run.run_id, assistant)
            .unwrap();
        let store = JsonlStore::open(&session_file).unwrap();
        let attempt_id = store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::StepAttempt {
                    run_id,
                    result_entry_id,
                    ..
                } if run_id == &run.run_id => Some(result_entry_id.clone()),
                _ => None,
            })
            .unwrap();

        assert_eq!(entry_id, attempt_id);
        assert!(store.entries().iter().any(|entry| entry.id == attempt_id));
        assert!(Reducer::reduce(&store).is_ok());
    }

    #[test]
    fn concurrent_subagent_starts_share_one_sequence_allocator() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::write(&session_file, "").unwrap();

        let runs = std::thread::scope(|scope| {
            (0..8)
                .map(|index| {
                    let file = session_file.clone();
                    scope.spawn(move || {
                        let mut journal = HarnessJournal::open(&file).unwrap();
                        journal
                            .start_subagent_lane(
                                &format!("subagent-1:{index}"),
                                &format!("task {index}"),
                                Some("node_1"),
                            )
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(
            runs.iter()
                .map(|run| run.run_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            runs.len()
        );
        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(store.records().len(), 24);
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::SubagentLifecycle { .. }))
                .count(),
            runs.len()
        );
    }

    #[test]
    fn safe_replay_claim_survives_fresh_journal_restore() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();
        let records = recover_v2_subagent_records(&session_file).unwrap();
        let pending = threadlane_agent::interrupted_subagent_lanes(&records);
        assert_eq!(pending[0].safe_tools.len(), 1);
        assert_eq!(
            journal
                .claim_safe_replays(&pending[0].safe_tools)
                .unwrap()
                .len(),
            1
        );
        drop(journal);

        let mut restored = HarnessJournal::open(&session_file).unwrap();
        let records = recover_v2_subagent_records(&session_file).unwrap();
        let pending = threadlane_agent::interrupted_subagent_lanes(&records);
        assert!(pending[0].safe_tools.is_empty());
        assert_eq!(pending[0].unsafe_tools.len(), 1);
        assert!(restored
            .claim_safe_replays(&pending[0].safe_tools)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cancellation_rejects_racing_start_and_writes_one_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();

        let _guard = abort_open_subagent_operations(&session_file).unwrap();
        journal
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(
                    record,
                    HarnessRecord::OperationFinished { run_id, .. }
                        if run_id == &identity.run_id
                ))
                .count(),
            1
        );
        assert!(matches!(
            store.records().last(),
            Some(HarnessRecord::OperationFinished {
                outcome: OperationOutcome::Aborted,
                ..
            })
        ));
    }

    #[test]
    fn subagent_journal_persists_start_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();

        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect the repository", None)
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(matches!(
            store.records().first(),
            Some(HarnessRecord::OperationStarted { id, .. })
                if id == &identity.run_id
        ));
        assert!(matches!(
            store.records().get(1),
            Some(HarnessRecord::StepAttempt { .. })
        ));
    }

    #[test]
    fn subagent_journal_tool_started_uses_explicit_empty_anchor_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();

        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::ToolStarted {
                assistant_entry_id,
                ..
            } if !assistant_entry_id.is_empty()
        )));
    }

    #[test]
    fn subagent_journal_checkpoint_skips_system_messages() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let messages = [
            AgentMessage::System {
                content: "system".into(),
            },
            AgentMessage::User {
                content: "task".into(),
            },
            AgentMessage::Assistant {
                content: Some("done".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            },
        ];

        let identity = journal
            .start_subagent_lane("subagent-1:0", "task", None)
            .unwrap();
        journal
            .checkpoint(&identity.lane_name, &identity.run_id, &messages)
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn subagent_journal_finish_closes_started_run() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();

        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(matches!(
            store.records().last(),
            Some(HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            }) if run_id == &identity.run_id
        ));
    }

    #[test]
    fn subagent_journal_finish_does_not_duplicate_across_loaded_journals() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut first = HarnessJournal::open(&session_file).unwrap();
        let identity = first
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        let mut second = HarnessJournal::open(&session_file).unwrap();

        first
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();
        second
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OperationOutcome::Aborted,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        let terminals: Vec<_> = store
            .records()
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    HarnessRecord::OperationFinished { run_id, .. }
                        if run_id == &identity.run_id
                )
            })
            .collect();
        assert_eq!(terminals.len(), 1);
        assert!(matches!(
            terminals.first(),
            Some(HarnessRecord::OperationFinished {
                outcome: OperationOutcome::Completed,
                ..
            })
        ));
    }

    #[test]
    fn interrupted_subagent_recovery_does_not_mutate_parent_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "write_file",
                serde_json::json!({}),
            )
            .unwrap();

        let mut tree = SessionTree::new("session");
        tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });
        assert_eq!(tree.get_active_branch_messages().len(), 1);
    }

    #[tokio::test]
    async fn safe_subagent_recovery_replays_tool_once() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let recovered_file = dir.path().join("recovered.txt");
        std::fs::write(&recovered_file, "replayed content").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[AgentMessage::User {
                    content: "deferred".into(),
                }],
            )
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": recovered_file}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let executor_count = Arc::new(AtomicU64::new(0));
        coding_agent
            .agent
            .hook_registry
            .register(
                HookKind::BeforeTool,
                "counting-before",
                counting_before_tool_handler(executor_count.clone()),
            )
            .ok();
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        let parent = coding_agent.session_tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });

        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            coding_agent.session_tree.active_node_id(),
            Some(parent.as_str())
        );
        assert_eq!(executor_count.load(Ordering::SeqCst), 0);
        assert!(coding_agent.session_tree.nodes.values().any(|node| {
            matches!(
                &node.message,
                AgentMessage::Custom { custom_type, payload }
                    if custom_type == "subagent_lane"
                        && payload.get("run_id").and_then(Value::as_str)
                            == Some(identity.run_id.as_str())
            )
        }));

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn interrupted_subagent_recovery_resumes_child_from_latest_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "finish the audit", None)
            .unwrap();
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[
                    AgentMessage::User {
                        content: "finish the audit".into(),
                    },
                    AgentMessage::Assistant {
                        content: Some("I inspected the first half.".into()),
                        tool_calls: None,
                        stop_reason: None,
                        deferred_handle: None,
                    },
                ],
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.set_subagent_work_observer(observed.clone());
        let mut events = coding_agent.agent.subscribe();

        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::RequestTurn(
                "Continue from the recovered checkpoint and finish the assigned task.".into()
            )]
        );
        assert!(coding_agent.session_tree.nodes.values().any(|node| {
            matches!(
                &node.message,
                AgentMessage::Assistant {
                    content: Some(content),
                    ..
                } if content == "test subagent result"
            )
        }));
        let recovery_statuses = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentEvent::SubagentRecovery { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            recovery_statuses,
            vec![
                threadlane_agent::SubagentRecoveryStatus::Started,
                threadlane_agent::SubagentRecoveryStatus::Retrying,
                threadlane_agent::SubagentRecoveryStatus::Recovered,
            ]
        );
    }

    #[tokio::test]
    async fn recovered_subagent_branch_uses_persisted_parent_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut original = CodingAgent::new(options);
        let parent = original.session_tree.add_message(AgentMessage::User {
            content: "originating parent".into(),
        });
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "finish the audit", Some(&parent))
            .unwrap();
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[AgentMessage::User {
                    content: "finish the audit".into(),
                }],
            )
            .unwrap();
        drop(original);
        drop(journal);

        let mut restarted_options = coding_agent_options(dir.path().to_path_buf());
        restarted_options.session_file = Some(session_file);
        let mut restarted = CodingAgent::new(restarted_options);
        restarted.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        assert_eq!(
            restarted
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            restarted.session_tree.active_node_id(),
            Some(parent.as_str())
        );
        let marker = restarted
            .session_tree
            .nodes
            .values()
            .find(|node| {
                matches!(
                    &node.message,
                    AgentMessage::Custom { custom_type, payload }
                        if custom_type == "subagent_lane"
                            && payload.get("run_id").and_then(Value::as_str)
                                == Some(identity.run_id.as_str())
                )
            })
            .unwrap();
        assert_eq!(marker.parent_id.as_deref(), Some(parent.as_str()));
    }

    #[tokio::test]
    async fn materialized_open_subagent_recovery_resumes_without_replaying_safe_tool() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let recovered_file = dir.path().join("recovered.txt");
        std::fs::write(&recovered_file, "replayed content").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": recovered_file}),
            )
            .unwrap();
        let records = recover_v2_subagent_records(&session_file).unwrap();
        let safe_tool = threadlane_agent::interrupted_subagent_lanes(&records)
            .remove(0)
            .safe_tools
            .remove(0);
        let executor_count = Arc::new(AtomicU64::new(0));

        let mut first_options = coding_agent_options(dir.path().to_path_buf());
        first_options.session_file = Some(session_file.clone());
        let mut first_agent = CodingAgent::new(first_options);
        first_agent
            .agent
            .hook_registry
            .register(
                HookKind::BeforeTool,
                "counting-before",
                counting_before_tool_handler(executor_count.clone()),
            )
            .ok();
        let safe_message = first_agent
            .replay_safe_tools(&[safe_tool])
            .await
            .into_iter()
            .map(|result| {
                let terminate = result.terminates();
                AgentMessage::Tool {
                    tool_call_id: result.tool_call_id,
                    name: result.name,
                    content: result.content,
                    is_error: result.is_error,
                    terminate,
                }
            })
            .next()
            .unwrap();
        assert_eq!(executor_count.load(Ordering::SeqCst), 0);
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[safe_message.clone()],
            )
            .unwrap();
        journal.refresh().unwrap();
        journal
            .store
            .finish_existing_tool(
                &identity.run_id,
                HarnessToolResult {
                    call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "1:6de|replayed content".into(),
                    is_error: false,
                    terminate: false,
                },
            )
            .unwrap();
        journal.store.drive_to_completion().unwrap();
        first_agent
            .session_tree
            .append_passive_branch_in_memory(
                None,
                vec![
                    AgentMessage::Custom {
                        custom_type: "subagent_lane".into(),
                        payload: serde_json::json!({
                            "lane": identity.lane_name.clone(),
                            "run_id": identity.run_id.clone(),
                            "agent": "recovered",
                            "task": "inspect",
                            "status": "completed",
                            "error": null,
                        }),
                    },
                    AgentMessage::User {
                        content: "deferred".into(),
                    },
                    safe_message,
                ],
            )
            .unwrap();
        drop(first_agent);
        drop(journal);

        let mut resumed_options = coding_agent_options(dir.path().to_path_buf());
        resumed_options.session_file = Some(session_file.clone());
        let mut resumed_agent = CodingAgent::new(resumed_options);
        resumed_agent
            .agent
            .hook_registry
            .replace(
                HookKind::BeforeTool,
                "counting-before",
                counting_before_tool_handler(executor_count.clone()),
            )
            .unwrap();
        assert_eq!(
            resumed_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(executor_count.load(Ordering::SeqCst), 0);
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished { run_id, .. } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn journal_load_failure_blocks_normal_input() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("invalid/session.jsonl");
        std::fs::create_dir_all(&session_file).unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let mut coding_agent = CodingAgent::new(options);

        let first_error = coding_agent
            .handle_input_with_images("/subagent", Vec::new())
            .await
            .unwrap()
            .unwrap_err();

        assert!(first_error.contains("Harness Error"));
        assert!(coding_agent.session_tree.nodes.is_empty());
    }

    #[tokio::test]
    async fn mixed_subagent_recovery_aborts_unsafe_tool_after_safe_replay() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let run_command_count = dir.path().join("run-command-count");
        let recovered_file = dir.path().join("recovered.txt");
        std::fs::write(&recovered_file, "replayed content").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "safe-call",
                "read_file",
                serde_json::json!({"path": recovered_file}),
            )
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "unsafe-call",
                "run_command",
                serde_json::json!({"command": format!("printf 1 >> {}", run_command_count.display())}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let safe_executor_count = Arc::new(AtomicU64::new(0));
        coding_agent
            .agent
            .hook_registry
            .register(
                HookKind::BeforeTool,
                "counting-before",
                counting_before_tool_handler(safe_executor_count.clone()),
            )
            .unwrap();
        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(safe_executor_count.load(Ordering::SeqCst), 0);
        assert!(!run_command_count.exists());
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Aborted,
                ..
            } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn unsafe_subagent_recovery_aborts_without_execution() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let run_command_count = dir.path().join("run-command-count");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "run_command",
                serde_json::json!({"command": format!("printf 1 >> {}", run_command_count.display())}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let mut events = coding_agent.agent.subscribe();

        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert!(!run_command_count.exists());

        let recovery_statuses = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentEvent::SubagentRecovery { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            recovery_statuses,
            vec![
                threadlane_agent::SubagentRecoveryStatus::Started,
                threadlane_agent::SubagentRecoveryStatus::Aborted,
            ]
        );

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Aborted,
                ..
            } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn recovery_failure_after_started_emits_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "unsafe-call",
                "run_command",
                serde_json::json!({"command": "pwd"}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let coding_agent = CodingAgent::new(options.clone());
        let mut events = coding_agent.agent.subscribe();
        // Cause open error by creating directory at invalid path
        options.session_file = Some(dir.path().join("invalid/session.jsonl"));
        std::fs::create_dir_all(options.session_file.as_ref().unwrap()).unwrap();
        let mut failing_agent = CodingAgent::new(options);

        let error = failing_agent
            .recover_interrupted_subagent_lanes()
            .await
            .unwrap_err();
        assert!(error.contains("Harness Error"));
        let recovery_statuses = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentEvent::SubagentRecovery { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(recovery_statuses.is_empty());
    }

    #[tokio::test]
    async fn model_switch_repairs_tool_call_interrupted_by_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let user = AgentMessage::User {
            content: "inspect the repository".into(),
        };
        coding_agent.session_tree.add_message(user.clone());
        {
            let mut state = coding_agent.agent.turn.lock().await;
            state.messages.push(user);
            state.messages.push(AgentMessage::Custom {
                custom_type: "thinking".into(),
                payload: serde_json::json!({"text": "planning"}),
            });
            state.messages.push(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-interrupted",
                    "read_file",
                    serde_json::json!({"path": "src/main.rs"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            });
        }

        let output = coding_agent.handle_input("/model next-model").await;

        assert_eq!(output.unwrap().unwrap(), "Switched model to: next-model");
        let state = coding_agent.agent.get_state().await;
        assert_eq!(state.model, "next-model");
        assert_eq!(state.messages.len(), 2);
        assert!(matches!(state.messages[1], AgentMessage::User { .. }));
        assert_eq!(
            coding_agent.session_tree.get_active_branch_messages().len(),
            1
        );
        let (_, codex) = coding_agent.agent.build_api_payloads().await;
        assert!(codex["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] != "function_call"));
    }

    #[tokio::test]
    async fn model_switch_preserves_antigravity_provider_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let output = coding_agent
            .handle_input("/model antigravity/gemini-3.6-flash")
            .await;

        assert_eq!(
            output.unwrap().unwrap(),
            "Switched model to: antigravity/gemini-3.6-flash"
        );
        let (chat, codex) = coding_agent.agent.build_api_payloads().await;
        assert_eq!(chat["model"], "antigravity/gemini-3.6-flash");
        assert_eq!(codex["model"], "antigravity/gemini-3.6-flash");
        assert_eq!(
            coding_agent.session_tree.model.as_deref(),
            Some("antigravity/gemini-3.6-flash")
        );
    }

    #[tokio::test]
    async fn invalid_command_returns_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let output = coding_agent.handle_input("/subagent").await;

        assert!(output.unwrap().is_err());
    }

    #[tokio::test]
    async fn persisted_session_history_is_loaded_into_provider_context() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let messages = vec![
            AgentMessage::User {
                content: "Choose a scrollbar behavior".into(),
            },
            AgentMessage::Assistant {
                content: Some("A. Always visible\nB. Visible while scrolling\nC. Hidden".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::User {
                content: "B".into(),
            },
        ];
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        for message in &messages {
            tree.add_message(message.clone());
        }

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let coding_agent = CodingAgent::new(options);

        let state = coding_agent.agent.get_state().await;
        assert!(matches!(
            state.messages.first(),
            Some(AgentMessage::System { .. })
        ));
        assert_eq!(
            serde_json::to_value(&state.messages[1..]).unwrap(),
            serde_json::to_value(&messages).unwrap()
        );

        let (chat, codex) = coding_agent.agent.build_api_payloads().await;
        assert_eq!(chat["messages"][2]["role"], "assistant");
        assert_eq!(chat["messages"][3]["content"], "B");
        assert_eq!(codex["input"][1]["role"], "assistant");
        assert_eq!(codex["input"][2]["content"][0]["text"], "B");
    }

    #[tokio::test]
    async fn sync_session_history_loads_recovered_messages_into_provider_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.session_tree.add_message(AgentMessage::User {
            content: "Recovered prompt".into(),
        });

        coding_agent.sync_session_history().await;

        let state = coding_agent.agent.get_state().await;
        assert!(matches!(
            state.messages.last(),
            Some(AgentMessage::User { content }) if content == "Recovered prompt"
        ));
    }

    #[tokio::test]
    async fn replay_safe_tools_executes_read_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovered.txt");
        std::fs::write(&path, "replayed content").unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let live_intents = Arc::new(AtomicU64::new(0));
        let observed_live_intents = live_intents.clone();
        coding_agent.set_tool_intent_recorder(Some(Arc::new(move |_, _, _| {
            let observed_live_intents = observed_live_intents.clone();
            Box::pin(async move {
                observed_live_intents.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })));
        let record = threadlane_agent::Record::ToolStarted {
            id: "tool-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run-1".into(),
            assistant_entry_id: String::new(),
            tool_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "read_file".into(),
            effective_args: serde_json::json!({"path": path}),
            result_entry_id: "result-1".into(),
            replay: threadlane_agent::ToolReplaySafety::Safe,
        };

        let results = coding_agent.replay_safe_tools(&[record]).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_call_id, "call-1");
        assert!(results[0].content.contains("replayed content"));
        assert!(!results[0].is_error);
        assert_eq!(live_intents.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn persisted_session_model_overrides_constructor_default() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        tree.add_message(AgentMessage::User {
            content: "continue".into(),
        });
        tree.set_model("antigravity/claude-opus-4-6".into())
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.model = "fallback-model".into();
        options.session_file = Some(session_file);
        let coding_agent = CodingAgent::new(options);

        assert_eq!(
            coding_agent.session_tree.model.as_deref(),
            Some("antigravity/claude-opus-4-6")
        );
        assert_eq!(
            coding_agent.agent.get_state().await.model,
            "antigravity/claude-opus-4-6"
        );
    }

    #[tokio::test]
    async fn v2_model_fact_overrides_legacy_metadata_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.model = "legacy-model".into();
        options.session_file = Some(session_file.clone());
        let mut first = CodingAgent::new(options);
        first
            .set_model("antigravity/provider-model".into())
            .await
            .unwrap();
        drop(first);

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.model = "constructor-default".into();
        options.session_file = Some(session_file);
        let restarted = CodingAgent::new(options);
        assert_eq!(
            restarted.session_tree.model.as_deref(),
            Some("antigravity/provider-model")
        );
        assert_eq!(
            restarted.agent.get_state().await.model,
            "antigravity/provider-model"
        );
    }

    #[tokio::test]
    async fn new_session_path_sets_unique_runtime_identity_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("sessions/session-42.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());

        let mut coding_agent = CodingAgent::new(options);

        assert_eq!(coding_agent.session_tree.session_id, "session-42");
        coding_agent.session_tree.add_message(AgentMessage::User {
            content: "persist me".into(),
        });
        assert_eq!(
            serde_json::to_value(
                SessionTree::load_from_file(&session_file)
                    .unwrap()
                    .get_active_branch_messages()
            )
            .unwrap(),
            serde_json::json!([{
                "role": "user",
                "content": "persist me",
            }])
        );
    }

    #[tokio::test]
    async fn v2_reload_uses_harness_leaf_when_metadata_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        let first = tree.add_message(AgentMessage::User {
            content: "first".into(),
        });
        let second = tree.add_message(AgentMessage::Assistant {
            content: Some("second".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });
        assert_ne!(first, second);

        let contents = fs::read_to_string(&session_file).unwrap();
        let stale = contents
            .lines()
            .map(|line| {
                if line.contains("\"type\":\"session_metadata\"") {
                    serde_json::from_str::<Value>(line)
                        .map(|mut value| {
                            value["active_node_id"] = Value::String(first.clone());
                            serde_json::to_string(&value).unwrap()
                        })
                        .unwrap()
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&session_file, format!("{stale}\n")).unwrap();

        let mut harness = AgentHarness::new(JsonlStore::open(&session_file).unwrap());
        harness
            .start_operation("run-1", Some(first), OperationIntent::Run)
            .unwrap();
        harness.drive_to_completion().unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let coding_agent = CodingAgent::new(options);
        assert_eq!(
            coding_agent.session_tree.active_node_id(),
            Some(second.as_str())
        );
    }

    #[test]
    fn subagent_ui_events_do_not_override_parent_lifecycle() {
        assert!(subagent_ui_event(AgentEvent::AgentStart, "child:").is_none());
        assert!(subagent_ui_event(
            AgentEvent::AgentEnd {
                usage: Default::default()
            },
            "child:"
        )
        .is_none());
        assert!(subagent_ui_event(
            AgentEvent::AgentError {
                error: "child failed".into()
            },
            "child:"
        )
        .is_none());
        assert!(subagent_ui_event(
            AgentEvent::SubagentQueued {
                run_id: 1,
                task_index: 0,
                agent: "nested".into(),
                task: "nested task".into(),
            },
            "child:"
        )
        .is_none());

        let reasoning = subagent_ui_event(
            AgentEvent::MessageUpdate {
                text_delta: Some("hidden child prose".into()),
                reasoning_delta: Some("visible progress".into()),
                tool_call_name: None,
            },
            "child:",
        );
        assert!(reasoning.is_none());

        let tool = subagent_ui_event(
            AgentEvent::ToolExecutionStart {
                tool_call_id: "tool".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
            "child:",
        );
        assert!(matches!(
            tool,
            Some(AgentEvent::ToolExecutionStart { tool_call_id, .. })
                if tool_call_id == "child:tool"
        ));
    }

    fn handler(capability: &'static str, work_dir: PathBuf) -> HostCapabilityHandler {
        handler_with_scheduler(capability, work_dir, AgentWorkScheduler::default())
    }

    fn handler_with_scheduler(
        capability: &'static str,
        work_dir: PathBuf,
        agent_work: AgentWorkScheduler,
    ) -> HostCapabilityHandler {
        let (event_tx, _) = broadcast::channel(4);
        HostCapabilityHandler {
            capability,
            tool_policy: None,
            extensions: Arc::new(WasiExtensionManager::new()),
            work_dir: work_dir.clone(),
            event_tx,
            allowed_hosts: Arc::new(HashSet::new()),
            permissions: None,
            agent_work,
            agent_runner: None,
            session_file: Some(work_dir.join("session.jsonl")),
            persist_tool_policy: false,
            managed_processes: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            bytes.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        push_unsigned_leb(payload.len() as u32, wasm);
        wasm.extend_from_slice(payload);
    }

    fn queue_command_wasm() -> Vec<u8> {
        let manifest = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "name": "queue_command_ext",
            "version": "1.0.0",
            "description": "scheduler integration fixture",
            "capabilities": ["agent"],
            "commands": [{"name": "queue", "description": "queue follow-up"}]
        })
        .to_string();
        let request = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "capability": "agent",
            "operation": "queue_message",
            "arguments": {"content": "standalone queued work"}
        })
        .to_string();
        let response = br#"{"message":"queued"}"#;
        let response_offset = 1024usize;
        let request_offset = 4096usize;
        let request_response_offset = 6000usize;
        let mut data = vec![0; request_response_offset + 1024];
        data[..manifest.len()].copy_from_slice(manifest.as_bytes());
        data[response_offset..response_offset + response.len()].copy_from_slice(response);
        data[request_offset..request_offset + request.len()].copy_from_slice(request.as_bytes());

        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(
            &mut wasm,
            1,
            &[
                4, 0x60, 0, 1, 0x7e, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e, 0x60, 4,
                0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f,
            ],
        );
        let host_module = b"threadlane_host";
        let mut imports = vec![1];
        push_unsigned_leb(host_module.len() as u32, &mut imports);
        imports.extend_from_slice(host_module);
        imports.push(7);
        imports.extend_from_slice(b"request");
        imports.extend_from_slice(&[0, 3]);
        push_section(&mut wasm, 2, &imports);
        push_section(&mut wasm, 3, &[3, 0, 1, 2]);
        push_section(&mut wasm, 5, &[1, 0, 2]);

        let mut exports = vec![4];
        for (name, kind, index) in [
            ("extension_info", 0, 1),
            ("alloc", 0, 2),
            ("execute_command", 0, 3),
            ("memory", 2, 0),
        ] {
            push_unsigned_leb(name.len() as u32, &mut exports);
            exports.extend_from_slice(name.as_bytes());
            exports.extend_from_slice(&[kind, index]);
        }
        push_section(&mut wasm, 7, &exports);

        let mut bodies = Vec::new();
        for body in [
            {
                let mut body = vec![0, 0x42];
                push_signed_leb(manifest.len() as i64, &mut body);
                body.push(0x0b);
                body
            },
            vec![0, 0x41, 0],
            {
                let mut body = vec![0, 0x41];
                push_signed_leb(request_offset as i64, &mut body);
                body.push(0x41);
                push_signed_leb(request.len() as i64, &mut body);
                body.push(0x41);
                push_signed_leb(request_response_offset as i64, &mut body);
                body.push(0x41);
                push_signed_leb(1024, &mut body);
                body.extend_from_slice(&[0x10, 0, 0x1a, 0x42]);
                let packed = ((response_offset as u64) << 32) | response.len() as u64;
                push_signed_leb(packed as i64, &mut body);
                body.push(0x0b);
                body
            },
        ] {
            let mut full = body;
            if full.last() != Some(&0x0b) {
                full.push(0x0b);
            }
            push_unsigned_leb(full.len() as u32, &mut bodies);
            bodies.extend_from_slice(&full);
        }
        let mut code = vec![3];
        code.extend_from_slice(&bodies);
        push_section(&mut wasm, 10, &code);
        let mut data_section = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(data.len() as u32, &mut data_section);
        data_section.extend_from_slice(&data);
        push_section(&mut wasm, 11, &data_section);
        wasm
    }

    const CONTINUATION_EXTENSION_NAME: &str = "continuation_tool_ext";
    const CONTINUATION_TOOL_NAME: &str = "continuation_tool";
    const CONTINUATION_TOOL_ARGS: &str = r#"{"sentinel":"same args"}"#;

    fn broker_tool_wasm(
        operation: &str,
        continue_after_broker: bool,
        finish_after_event: bool,
    ) -> Vec<u8> {
        let manifest = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "name": CONTINUATION_EXTENSION_NAME,
            "version": "1.0.0",
            "description": "broker continuation fixture",
            "capabilities": ["tools"],
            "tools": [{
                "name": CONTINUATION_TOOL_NAME,
                "description": "exercise broker continuation",
                "parameters": {"type": "object"}
            }]
        })
        .to_string();
        let request = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "capability": "tools",
            "operation": operation,
            "arguments": Value::Null
        })
        .to_string();
        let initial_response = serde_json::json!({
            "message": "waiting for broker response",
            "continue_after_broker": continue_after_broker
        })
        .to_string();
        let final_response = serde_json::json!({
            "message": "post-processed broker response"
        })
        .to_string();
        let initial_invocation_len = serde_json::to_vec(&WasiExtensionInvocation {
            api_version: BROKER_API_VERSION,
            kind: "tool".into(),
            name: CONTINUATION_TOOL_NAME.into(),
            arguments: serde_json::from_str(CONTINUATION_TOOL_ARGS).unwrap(),
            state: serde_json::json!({}),
            events: Vec::new(),
        })
        .unwrap()
        .len();

        let initial_response_offset = 1024usize;
        let final_response_offset = 2048usize;
        let request_offset = 4096usize;
        let request_response_offset = 6144usize;
        let mut data = vec![0; request_response_offset + 1024];
        data[..manifest.len()].copy_from_slice(manifest.as_bytes());
        data[initial_response_offset..initial_response_offset + initial_response.len()]
            .copy_from_slice(initial_response.as_bytes());
        data[final_response_offset..final_response_offset + final_response.len()]
            .copy_from_slice(final_response.as_bytes());
        data[request_offset..request_offset + request.len()].copy_from_slice(request.as_bytes());

        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(
            &mut wasm,
            1,
            &[
                4, 0x60, 0, 1, 0x7e, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e, 0x60, 4,
                0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f,
            ],
        );
        let host_module = b"threadlane_host";
        let mut imports = vec![1];
        push_unsigned_leb(host_module.len() as u32, &mut imports);
        imports.extend_from_slice(host_module);
        imports.push(7);
        imports.extend_from_slice(b"request");
        imports.extend_from_slice(&[0, 3]);
        push_section(&mut wasm, 2, &imports);
        push_section(&mut wasm, 3, &[3, 0, 1, 2]);
        push_section(&mut wasm, 5, &[1, 0, 2]);

        let mut exports = vec![4];
        for (name, kind, index) in [
            ("extension_info", 0, 1),
            ("alloc", 0, 2),
            ("execute_tool", 0, 3),
            ("memory", 2, 0),
        ] {
            push_unsigned_leb(name.len() as u32, &mut exports);
            exports.extend_from_slice(name.as_bytes());
            exports.extend_from_slice(&[kind, index]);
        }
        push_section(&mut wasm, 7, &exports);

        let mut extension_info = vec![0, 0x42];
        push_signed_leb(manifest.len() as i64, &mut extension_info);
        extension_info.push(0x0b);
        let alloc = vec![0, 0x41, 0, 0x0b];
        let mut execute_tool = vec![0];
        if finish_after_event {
            execute_tool.extend_from_slice(&[0x20, 1, 0x41]);
            push_signed_leb(initial_invocation_len as i64, &mut execute_tool);
            execute_tool.extend_from_slice(&[0x4b, 0x04, 0x7e, 0x42]);
            let packed = ((final_response_offset as u64) << 32) | final_response.len() as u64;
            push_signed_leb(packed as i64, &mut execute_tool);
            execute_tool.push(0x05);
        }
        execute_tool.push(0x41);
        push_signed_leb(request_offset as i64, &mut execute_tool);
        execute_tool.push(0x41);
        push_signed_leb(request.len() as i64, &mut execute_tool);
        execute_tool.push(0x41);
        push_signed_leb(request_response_offset as i64, &mut execute_tool);
        execute_tool.push(0x41);
        push_signed_leb(1024, &mut execute_tool);
        execute_tool.extend_from_slice(&[0x10, 0, 0x1a, 0x42]);
        let packed = ((initial_response_offset as u64) << 32) | initial_response.len() as u64;
        push_signed_leb(packed as i64, &mut execute_tool);
        if finish_after_event {
            execute_tool.push(0x0b);
        }
        execute_tool.push(0x0b);

        let mut code = vec![3];
        for body in [extension_info, alloc, execute_tool] {
            push_unsigned_leb(body.len() as u32, &mut code);
            code.extend_from_slice(&body);
        }
        push_section(&mut wasm, 10, &code);
        let mut data_section = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(data.len() as u32, &mut data_section);
        data_section.extend_from_slice(&data);
        push_section(&mut wasm, 11, &data_section);
        wasm
    }

    fn coding_agent_options(work_dir: PathBuf) -> CodingAgentOptions {
        CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir,
            session_file: None,
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
        }
    }

    #[test]
    fn set_credentials_updates_the_running_agent() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        agent.set_credentials("new-token".into(), Some("new-account".into()));

        assert_eq!(agent.agent.api_key, "new-token");
        assert_eq!(agent.agent.account_id.as_deref(), Some("new-account"));
    }

    #[test]
    fn cancel_keeps_subagent_cancellation_active_until_the_next_submission() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let agent = CodingAgent::new(options);
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let mut events = agent.subscribe();

        agent.cancel().unwrap();

        assert!(journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .is_err());
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::AgentError { error }) if error == "Generation cancelled"
        ));
    }

    #[tokio::test]
    async fn cancel_aborts_active_run_without_a_session_file_before_the_next_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let cancellation = agent.cancellation_handle();
        let mut events = agent.subscribe();
        let event_tx = agent.agent.event_tx.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let run = tokio::spawn(async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            let _ = event_tx.send(AgentEvent::MessageUpdate {
                text_delta: Some("late".into()),
                reasoning_delta: None,
                tool_call_name: None,
            });
        });
        started_rx.await.unwrap();
        cancellation.track_active_run(run.abort_handle()).unwrap();

        cancellation.cancel().unwrap();

        assert!(run.await.unwrap_err().is_cancelled());
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::AgentError { error }) if error == "Generation cancelled"
        ));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        let event_tx = agent.agent.event_tx.clone();
        let next = tokio::spawn(async move {
            let _ = event_tx.send(AgentEvent::MessageUpdate {
                text_delta: Some("next".into()),
                reasoning_delta: None,
                tool_call_name: None,
            });
        });
        cancellation.track_active_run(next.abort_handle()).unwrap();
        next.await.unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::MessageUpdate { text_delta: Some(text), .. }) if text == "next"
        ));
    }

    fn provider_tool_call(
        id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> threadlane_provider::openai::ToolCall {
        threadlane_provider::openai::ToolCall {
            id: id.into(),
            r#type: "function".into(),
            function: threadlane_provider::openai::ToolCallFunction {
                name: name.into(),
                arguments: arguments.to_string(),
            },
            thought_signature: None,
        }
    }

    fn counting_before_tool_handler(count: Arc<AtomicU64>) -> HookHandler {
        Arc::new(move |_context: HookContext| {
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(HookEffect::default())
            })
        })
    }

    #[tokio::test]
    async fn coding_agent_builds_configurable_structured_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Always add focused tests.").unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.system_prompt = SystemPromptConfig {
            custom_prompt: Some("CUSTOM_BASE".into()),
            append_prompt: Some("APPENDED_RULE".into()),
            guidelines: Vec::new(),
        };

        let coding_agent = CodingAgent::new(options);
        let state = coding_agent.agent.get_state().await;

        assert!(state
            .system_prompt
            .starts_with("CUSTOM_BASE\n\nAPPENDED_RULE"));
        assert!(state.system_prompt.contains("<project_context>"));
        assert!(state.system_prompt.contains("Always add focused tests."));
        assert!(state.system_prompt.contains("Current working directory:"));
    }

    #[tokio::test]
    async fn coding_agent_advertises_and_executes_discovered_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".threadlane/skills/test-workflow");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-workflow\ndescription: Use for deterministic integration tests\n---\nBODY_SENTINEL",
        )
        .unwrap();

        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let state = coding_agent.agent.get_state().await;
        assert!(state.system_prompt.contains("`test-workflow`"));
        assert!(state
            .system_prompt
            .contains("Use for deterministic integration tests"));
        assert!(!state.system_prompt.contains("BODY_SENTINEL"));
        assert!(state.system_prompt.contains("- read_file:"));
        assert!(state.system_prompt.contains("- load_skill:"));

        let (chat, codex) = coding_agent.agent.build_api_payloads().await;
        assert!(chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["function"]["name"] == threadlane_skills::LOAD_SKILL_TOOL_NAME }));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == threadlane_skills::LOAD_SKILL_TOOL_NAME }));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == crate::plan::UPDATE_PLAN_TOOL_NAME }));

        let results = coding_agent
            .agent
            .execute_tools(&[provider_tool_call(
                "skill-call",
                threadlane_skills::LOAD_SKILL_TOOL_NAME,
                serde_json::json!({"name": "test-workflow"}),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);
        assert!(results[0].content.contains("BODY_SENTINEL"));
    }

    #[tokio::test]
    async fn coding_agent_restores_the_session_plan() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let coding_agent = CodingAgent::new(options);

        let results = coding_agent
            .agent
            .execute_tools(&[provider_tool_call(
                "plan-call",
                crate::plan::UPDATE_PLAN_TOOL_NAME,
                serde_json::json!({
                    "plan": [{"step": "Verify", "status": "in_progress"}]
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);

        let mut restored_options = coding_agent_options(dir.path().to_path_buf());
        restored_options.session_file = Some(session_file);
        let restored = CodingAgent::new(restored_options);
        assert_eq!(restored.current_plan().items[0].step, "Verify");
    }

    #[tokio::test]
    async fn model_subagent_tool_returns_awaited_child_output() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".threadlane/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();

        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        let mut lifecycle_events = coding_agent.subscribe();
        let (chat, codex) = coding_agent.agent.build_api_payloads().await;
        assert!(chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "subagent"));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "subagent"));

        let results = coding_agent
            .agent
            .execute_tools(&[provider_tool_call(
                "subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [
                        {"agent": "scout", "task": "inspect the project"},
                        {"agent": "reviewer", "task": "review the project"}
                    ],
                    "parallel": true
                }),
            )])
            .await;
        let events = std::iter::from_fn(|| lifecycle_events.try_recv().ok()).collect::<Vec<_>>();
        let queued = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::SubagentQueued {
                    run_id,
                    task_index,
                    agent,
                    task,
                } => Some((*run_id, *task_index, agent.clone(), task.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let started = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::SubagentStarted { .. }))
            .count();
        let finished = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::SubagentFinished { .. }))
            .count();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].0, queued[1].0);
        assert_eq!(queued[0].1, 0);
        assert_eq!(queued[1].1, 1);
        assert_eq!(started, 2);
        assert_eq!(finished, 2);
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error, "{}", results[0].content);
        assert!(results[0]
            .content
            .contains("test subagent result (test-model)"));
        assert!(!results[0].content.contains("Running 1 subagent task"));
    }

    #[tokio::test]
    async fn malformed_model_subagent_tool_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let results = coding_agent
            .agent
            .execute_tools(&[provider_tool_call(
                "invalid-subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [{"agent": "scout", "task": ""}],
                    "parallel": false
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
    }

    #[tokio::test]
    async fn standalone_extension_command_runs_scheduled_agent_work() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = queue_command_wasm();
        let extension_dir = dir.path().join(".threadlane/extensions/queue_command_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::write(extension_dir.join("extension.wasm"), wasm).unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(dir.path().join("session.jsonl"));
        let mut coding_agent = CodingAgent::new(options);
        assert!(coding_agent.wasi_extensions.has_command("queue"));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        let output = coding_agent.handle_input("/queue").await;

        assert_eq!(output.unwrap().unwrap(), "queued");
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage {
                content: "standalone queued work".into(),
                images: Vec::new(),
            }]
        );
    }

    #[tokio::test]
    async fn queued_follow_up_runs_through_the_agent_scheduler() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        coding_agent.work_handle().queue_follow_up_with_images(
            "interrupt the current turn",
            vec![ImageAttachment {
                display_name: "diagram.png".into(),
                data_url: "data:image/png;base64,AA==".into(),
            }],
        );
        coding_agent.run_scheduled_agent_work().await;

        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage {
                content: "interrupt the current turn".into(),
                images: vec![ImageAttachment {
                    display_name: "diagram.png".into(),
                    data_url: "data:image/png;base64,AA==".into(),
                }],
            }]
        );
    }

    #[tokio::test]
    async fn queued_follow_up_is_persisted_and_consumed_by_the_harness() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed);

        coding_agent
            .work_handle()
            .try_queue_follow_up_with_images("durable follow-up", Vec::new())
            .unwrap();
        let queued = JsonlStore::open(&session_file).unwrap();
        assert!(queued.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: QueueKind::FollowUp,
                ..
            }
        )));

        coding_agent.run_scheduled_agent_work().await;
        let consumed = JsonlStore::open(&session_file).unwrap();
        assert!(consumed
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::QueueConsumed { .. })));
        assert!(Reducer::reduce(&consumed)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn queued_steer_is_persisted_and_consumed_by_the_harness() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        coding_agent
            .work_handle()
            .queue_steer_with_images("durable steer", Vec::new())
            .unwrap();
        let queued = JsonlStore::open(&session_file).unwrap();
        assert!(queued.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: QueueKind::Steer,
                ..
            }
        )));

        coding_agent.run_scheduled_agent_work().await;
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::SteerMessage {
                content: "durable steer".into(),
                images: Vec::new(),
            }]
        );
        let consumed = JsonlStore::open(&session_file).unwrap();
        assert!(consumed
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::QueueConsumed { .. })));
        assert!(Reducer::reduce(&consumed)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn queued_next_run_is_persisted_and_consumed_by_the_harness() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        coding_agent
            .work_handle()
            .queue_next_run_with_images("durable next run", Vec::new())
            .unwrap();
        let queued = JsonlStore::open(&session_file).unwrap();
        assert!(queued.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: QueueKind::NextRun,
                ..
            }
        )));

        coding_agent.run_scheduled_agent_work().await;
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::NextRunMessage {
                content: "durable next run".into(),
                images: Vec::new(),
            }]
        );
        let consumed = JsonlStore::open(&session_file).unwrap();
        assert!(Reducer::reduce(&consumed)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn generic_agent_run_inherits_parent_current_model_for_tasks_without_model() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".threadlane/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.set_subagent_work_observer(observed.clone());
        coding_agent.agent.turn.lock().await.model = "changed-model".into();

        let output = coding_agent
            .handle_input("/subagent inspect the project")
            .await;

        assert!(output
            .unwrap()
            .unwrap()
            .contains("test subagent result (changed-model)"));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage {
                content: "test subagent follow-up".into(),
                images: Vec::new(),
            }]
        );
    }

    #[tokio::test]
    async fn dynamic_subagent_spawns_without_predefined_agent_file() {
        let dir = tempfile::tempdir().unwrap();
        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));

        let results = coding_agent
            .agent
            .execute_tools(&[provider_tool_call(
                "dynamic-subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [{
                        "agent": "custom_architect",
                        "task": "design architecture",
                        "instructions": "You are a custom architect.",
                        "tools": ["read_file"]
                    }],
                    "parallel": false
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error, "{}", results[0].content);
        assert!(results[0].content.contains("test subagent result"));
    }

    #[test]
    fn generic_tool_policy_state_restores_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(dir.path(), "session-a");
        manager
            .set_host_state("tools.policy", Value::String("read_only".into()))
            .unwrap();

        let restored = WasiExtensionManager::for_project_session(dir.path(), "session-a");
        assert_eq!(
            restored.host_state("tools.policy"),
            Some(Value::String("read_only".into()))
        );
    }

    #[test]
    fn tool_policy_is_unchanged_when_host_state_persistence_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".threadlane"), "not a directory").unwrap();
        let policy = Arc::new(tokio::sync::Mutex::new(ToolPolicy::FullAccess));
        let tools = HostCapabilityHandler {
            tool_policy: Some(policy.clone()),
            extensions: Arc::new(WasiExtensionManager::for_project_session(
                dir.path(),
                "session-a",
            )),
            persist_tool_policy: true,
            ..handler("tools", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: BROKER_API_VERSION,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: serde_json::json!({"policy": "read_only"}),
        };

        assert_eq!(tools.handle(&request).unwrap_err().code, "host_error");
        assert_eq!(*policy.try_lock().unwrap(), ToolPolicy::FullAccess);
    }

    #[test]
    fn filesystem_rejects_paths_outside_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let request = BrokerRequest {
            api_version: 2,
            capability: "fs".into(),
            operation: "read_text".into(),
            arguments: serde_json::json!({"path": "../outside"}),
        };
        let error = handler("fs", dir.path().to_path_buf())
            .handle(&request)
            .unwrap_err();
        assert_eq!(error.code, "invalid_argument");
    }

    struct RecordingBrokerHandler {
        operations: Arc<Mutex<Vec<String>>>,
    }

    impl CapabilityHandler for RecordingBrokerHandler {
        fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
            self.operations
                .lock()
                .unwrap()
                .push(request.operation.clone());
            if request.operation == "fail" {
                Err(BrokerError {
                    code: "test_error".into(),
                    message: "expected test failure".into(),
                })
            } else {
                Ok(Value::Null)
            }
        }
    }

    #[tokio::test]
    async fn tool_broker_requests_dispatch_in_order_and_isolate_errors() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let dispatcher = Arc::new(dispatcher);
        let requests = ["first", "fail", "last"]
            .into_iter()
            .map(|operation| crate::extension_broker::HostBrokerRequest {
                request: BrokerRequest {
                    api_version: BROKER_API_VERSION,
                    capability: "tools".into(),
                    operation: operation.into(),
                    arguments: Value::Null,
                },
                invoking_extension: "tool-ext".into(),
            })
            .collect();

        let extensions = WasiExtensionManager::new();
        dispatch_hook_requests_isolated(&dispatcher, &extensions, requests, "test broker error")
            .await;

        assert_eq!(*operations.lock().unwrap(), vec!["first", "fail", "last"]);
    }

    #[test]
    fn wasi_extension_response_defaults_continuation_to_false() {
        let response: WasiExtensionResponse =
            serde_json::from_value(serde_json::json!({"message": "legacy"})).unwrap();

        assert!(!response.continue_after_broker);
    }

    #[tokio::test]
    async fn wasi_tool_continuation_post_processes_broker_operation_errors() {
        let extension =
            WasiExtension::load_from_bytes(broker_tool_wasm("fail", true, true)).unwrap();
        let extensions = WasiExtensionManager::new();
        extensions.register_extension(extension).unwrap();
        let extensions = Arc::new(extensions);
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let executor = BrokerAwareWasiToolExecutor {
            extensions: extensions.clone(),
            broker_dispatcher: Arc::new(dispatcher),
        };

        let output = executor
            .execute_tool(CONTINUATION_TOOL_NAME, CONTINUATION_TOOL_ARGS)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(output, "post-processed broker response");
        assert_eq!(*operations.lock().unwrap(), vec!["fail"]);
        assert!(extensions
            .drain_events_for(CONTINUATION_EXTENSION_NAME)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn wasi_tool_without_continuation_preserves_queued_broker_results() {
        let extension =
            WasiExtension::load_from_bytes(broker_tool_wasm("fail", false, false)).unwrap();
        let extensions = WasiExtensionManager::new();
        extensions.register_extension(extension).unwrap();
        let extensions = Arc::new(extensions);
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let executor = BrokerAwareWasiToolExecutor {
            extensions: extensions.clone(),
            broker_dispatcher: Arc::new(dispatcher),
        };

        let error = executor
            .execute_tool(CONTINUATION_TOOL_NAME, CONTINUATION_TOOL_ARGS)
            .await
            .unwrap()
            .unwrap_err();

        assert_eq!(error, "expected test failure");
        assert_eq!(*operations.lock().unwrap(), vec!["fail"]);
        let events = extensions
            .drain_events_for(CONTINUATION_EXTENSION_NAME)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["error"]["code"], "test_error");
    }

    #[tokio::test]
    async fn wasi_tool_continuation_has_an_actionable_round_limit() {
        let extension =
            WasiExtension::load_from_bytes(broker_tool_wasm("loop", true, false)).unwrap();
        let extensions = WasiExtensionManager::new();
        extensions.register_extension(extension).unwrap();
        let extensions = Arc::new(extensions);
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let executor = BrokerAwareWasiToolExecutor {
            extensions,
            broker_dispatcher: Arc::new(dispatcher),
        };

        let error = executor
            .execute_tool(CONTINUATION_TOOL_NAME, CONTINUATION_TOOL_ARGS)
            .await
            .unwrap()
            .unwrap_err();

        assert_eq!(
            operations.lock().unwrap().len(),
            MAX_BROKER_CONTINUATION_ROUNDS
        );
        assert!(error.contains(CONTINUATION_TOOL_NAME));
        assert!(error.contains(&format!("{MAX_BROKER_CONTINUATION_ROUNDS} rounds")));
        assert!(error.contains("broker_response"));
    }

    #[test]
    fn process_run_limits_preserve_defaults_and_apply_hard_caps() {
        assert_eq!(
            process_run_limits(&serde_json::json!({})).unwrap(),
            ProcessRunLimits {
                timeout: CAPABILITY_TIMEOUT,
                max_output_bytes: MAX_CAPABILITY_BUFFER_BYTES,
            }
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({
                "timeout_ms": 1_234,
                "max_output_bytes": 4_096,
            }))
            .unwrap(),
            ProcessRunLimits {
                timeout: Duration::from_millis(1_234),
                max_output_bytes: 4_096,
            }
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({
                "timeout_ms": MAX_PROCESS_TIMEOUT_MS + 1,
                "max_output_bytes": MAX_PROCESS_OUTPUT_BYTES as u64 + 1,
            }))
            .unwrap(),
            ProcessRunLimits {
                timeout: Duration::from_millis(MAX_PROCESS_TIMEOUT_MS),
                max_output_bytes: MAX_PROCESS_OUTPUT_BYTES,
            }
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({"timeout_ms": 0}))
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({"max_output_bytes": "1024"}))
                .unwrap_err()
                .code,
            "invalid_argument"
        );
    }

    #[tokio::test]
    async fn process_pipes_output_and_timeout_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let mut request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "run".into(),
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf stdout; printf stderr >&2"]
            }),
        };
        let output = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output["stdout"], "stdout");
        assert_eq!(output["stderr"], "stderr");

        request.arguments = serde_json::json!({
            "program": "sh",
            "args": ["-c", "sleep 10"],
            "timeout_ms": 25
        });
        let started = Instant::now();
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < StdDuration::from_secs(1));
    }

    #[tokio::test]
    async fn process_output_is_bounded_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "run".into(),
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", format!("head -c {} /dev/zero", MAX_CAPABILITY_BUFFER_BYTES + 1)]
            }),
        };
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "process_output_too_large");

        let request = BrokerRequest {
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf 123456789"],
                "max_output_bytes": 8
            }),
            ..request
        };
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "process_output_too_large");
        assert!(error.message.contains("8-byte buffer limit"));
    }

    #[tokio::test]
    async fn managed_process_round_trips_one_content_length_message() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "spawn".into(),
            arguments: serde_json::json!({
                "name": "echo",
                "program": "cat",
                "args": []
            }),
        };
        process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();

        let request = BrokerRequest {
            operation: "send".into(),
            arguments: serde_json::json!({
                "name": "echo",
                "data": "Content-Length: 2\r\n\r\nok\n"
            }),
            ..request
        };
        process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();

        let request = BrokerRequest {
            operation: "recv".into(),
            arguments: serde_json::json!({
                "name": "echo",
                "framing": "content-length",
                "timeout_ms": 1_000
            }),
            ..request
        };
        let output = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output, serde_json::json!({"data": "ok", "eof": false}));
    }

    #[tokio::test]
    async fn managed_processes_are_private_to_the_invoking_extension() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "spawn".into(),
            arguments: serde_json::json!({"name": "private", "program": "cat", "args": []}),
        };
        process
            .handle_for_extension_async(&request, "owner")
            .await
            .unwrap();
        let output = process
            .handle_for_extension_async(
                &BrokerRequest {
                    operation: "status".into(),
                    arguments: serde_json::json!({"name": "private"}),
                    ..request
                },
                "other",
            )
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output["processes"], serde_json::json!([]));
    }

    #[test]
    fn filesystem_rename_stays_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.rs"), "fn old() {}").unwrap();
        let fs = handler("fs", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "fs".into(),
            operation: "rename".into(),
            arguments: serde_json::json!({"from": "old.rs", "to": "new.rs"}),
        };

        fs.handle(&request).unwrap();

        assert!(!dir.path().join("old.rs").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.rs")).unwrap(),
            "fn old() {}"
        );
    }

    #[tokio::test]
    async fn unattended_network_request_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let network = handler("network", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": "https://example.com/",
                "method": "GET",
                "body": ""
            }),
        };

        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "host_denied");
    }

    #[tokio::test]
    async fn network_response_is_bounded_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            let oversized_response = response.len() + MAX_CAPABILITY_BUFFER_BYTES + 1;
            response.resize(oversized_response, b'x');
            tokio::io::AsyncWriteExt::write_all(&mut socket, &response)
                .await
                .unwrap();
        });
        let network = HostCapabilityHandler {
            allowed_hosts: Arc::new(HashSet::from(["127.0.0.1".to_string()])),
            ..handler("network", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": format!("http://127.0.0.1:{port}/"),
                "method": "GET",
                "body": ""
            }),
        };
        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "network_response_too_large");
    }

    #[tokio::test]
    async fn network_io_timeout_is_bounded_after_connect() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let network = HostCapabilityHandler {
            allowed_hosts: Arc::new(HashSet::from(["127.0.0.1".to_string()])),
            ..handler("network", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": format!("http://127.0.0.1:{port}/"),
                "method": "GET",
                "body": ""
            }),
        };
        let started = Instant::now();
        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < StdDuration::from_secs(4));
    }
}
