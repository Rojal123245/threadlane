# Task 2 implementation report

## Delivered

- Routed `SubagentQueued`, `SubagentStarted`, `SubagentFinished`, and `SubagentRecovery` through the existing `GuiAgentEvent::GenerationAgent` path into the session-scoped `ChatData` reducer.
- Reused the supervisor's `subagent-<run>:<task-index>` lane key so queued, started, recovery, and finished updates replace one activity item.
- Preserved the existing stale-generation filter in `poll_agent_events`, session/composer state, and the existing post-poll chat redraw.
- Classified completed work as `Recovered`, provider failures as `Retrying` with bounded detail, cancellation as `Cancelled`, and unsafe interruption as `Aborted`.
- Review fix: `SubagentStarted` and `SubagentFinished` now carry the journal-generated `subagent-run-<sequence>` identity. The chat reducer migrates the temporary queued UI key to that durable identity, which is also used by `SubagentRecovery`.
- Review fix: a partial journal start now returns the persisted `OperationStarted` identity with its error. `SubagentFinished` therefore retains that durable ID when `TaskAttempt` cannot be appended, allowing later recovery to update the same activity.

## Tests and checks

- Red: `rtk cargo test -p threadlane --lib harness_event_routing` reported that `threadlane` has no library target. The equivalent binary-target test failed as expected because the lifecycle reducer bridge was absent.
- Pass: `rtk cargo test -p threadlane --bin threadlane harness_event_routing` — 1 passed.
- Pass: `rtk cargo test -p threadlane --bin threadlane harness_activity` — 6 passed.
- Pass: `rtk cargo test -p threadlane --bin threadlane harness_event_routing` — 1 passed using the journal identity format.
- Pass: `rtk cargo test -p threadlane-coding-agent model_subagent_tool_returns_awaited_child_output` — 1 passed.
- Pass: `rtk cargo test -p threadlane-coding-agent subagent_intents_are_durable_before_child_execution` — 1 passed.
- Pass: `rtk cargo test -p threadlane-coding-agent partial_journal_start_finishes_with_the_persisted_run_id` — 1 passed.
- Pass: `rtk cargo test -p threadlane --bin threadlane partial_journal_start_and_recovery_share_one_harness_activity` — 1 passed.
- Pass: `rtk cargo test -p threadlane --bin threadlane` — 128 passed.
- Pass: `rtk cargo check -p threadlane` — 0 errors; existing Makepad dependency and unrelated dead-code warnings remain.
- Pass: `rtk git -c core.fsmonitor=false diff --check`.

## Scope

No event bus, registry, persistence, dashboard, or rendering changes were added. Runtime visual verification is not needed for this routing-only slice; the existing chat-panel redraw path is retained unchanged.
