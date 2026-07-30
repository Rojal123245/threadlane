# In-Flight Subagent Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist running subagent intent and completed turn checkpoints so interrupted lanes recover idempotently without replaying unsafe tools.

**Architecture:** Add one shared per-session subagent journal inside `CodingAgent`, backed by the existing oplog helpers and `OpRecord` variants. Child lanes write `OperationStarted`, `TaskAttempt`, `ToolStarted`, and turn-boundary `WriteDeferred` records before finishing with `OperationFinished`. Recovery materializes deferred messages onto passive branches, replays only safe open tools, and aborts unsafe interruptions.

**Tech Stack:** Rust, Tokio, serde JSONL, existing `SessionTree`, `OpRecord`, `ToolIntentRecorder`, and replay classifier.

## Global Constraints

- Use the parent session `.oplog.jsonl`; add no persistence backend or dependency.
- Persist operation/task/tool intent before its associated model or tool execution.
- Use `WriteDeferred` only for finalized non-system messages at turn boundaries.
- Never replay `ToolReplaySafety::Never` tools.
- Safe tools may replay at most once.
- Recovery must be idempotent across repeated restores.
- Passive child checkpoints must not move the parent active leaf.
- Partial token deltas and frontend lane browsing remain out of scope.
- Use TDD for every behavior.

---

### Task 1: Add a shared subagent lane journal

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs`

**Interfaces:**

```rust
#[derive(Clone)]
struct SubagentLaneJournal {
    session_file: PathBuf,
    records: Arc<std::sync::Mutex<Vec<OpRecord>>>,
}

impl SubagentLaneJournal {
    fn load(session_file: &Path) -> Result<Self, String>;
    fn start(&self, lane: &str, run_id: &str, task: &str) -> Result<(), String>;
    fn tool_started(
        &self,
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: serde_json::Value,
    ) -> Result<(), String>;
    fn checkpoint(&self, lane: &str, run_id: &str, messages: &[AgentMessage])
        -> Result<(), String>;
    fn finish(&self, lane: &str, run_id: &str, outcome: OpOutcome, error: Option<String>)
        -> Result<(), String>;
}
```

- [ ] Write `subagent_journal_persists_start_before_returning` and assert `OperationStarted` then `TaskAttempt`, non-empty IDs, one run ID, and strictly increasing sequence numbers.
- [ ] Run `rtk cargo test -p threadlane-coding-agent subagent_journal_persists_start_before_returning`; expect compile failure because the journal is absent.
- [ ] Implement `load` with `load_op_records_from_file` and one shared records mutex. Each mutation must allocate the next sequence from the durable maximum under the append lock, append with `append_op_record_to_file`, then push in memory.
- [ ] Implement `checkpoint` as one `WriteDeferred` per message, skipping `AgentMessage::System`.
- [ ] Run the focused test and `rtk cargo test -p threadlane-coding-agent subagent_journal`; expect PASS.
- [ ] Commit with `git commit -m "feat: add subagent lane journal"`.

---

### Task 2: Record child intent and completed turns

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs`

**Interfaces:**
- Add `journal: Option<SubagentLaneJournal>` to `SubagentRunContext`.
- Extend `CompletedSubagentLane` with `run_id: String`.
- Reuse `AgentLoop::tool_intent_recorder`.

- [ ] Write `subagent_intents_are_durable_before_child_execution`. Use the deterministic child observer to inspect the oplog when child work begins; assert start/task records already exist.
- [ ] Write `subagent_tool_intent_failure_blocks_child_executor`. Configure a journal path whose parent is missing, invoke a deterministic tool, and assert the executor observer remains false.
- [ ] Run both focused tests; expect failures because child dispatch does not install journal intent.
- [ ] In `run_one`, allocate `run_id = "subagent-{ui_run_id}:{task_index}"`, call `journal.start` before `SubagentStarted`, and return a failed lane without invoking `run_subagent_task` when persistence fails.
- [ ] In `run_subagent_task`, install a recorder that calls `journal.tool_started` before any child tool execution.
- [ ] Subscribe to child `TurnEnd`; snapshot `agent.loop_engine.state`, checkpoint only newly completed non-system messages, and update the checkpoint cursor after a successful append.
- [ ] Persist the final snapshot after `agent.prompt` returns, then call `journal.finish` only after `CompletedSubagentLane` has been accepted by the completed-lane sink.
- [ ] Run `rtk cargo test -p threadlane-coding-agent subagent_intent` and `rtk cargo test -p threadlane-coding-agent subagent`; expect PASS.
- [ ] Commit with `git commit -m "feat: journal running subagent lanes"`.

---

### Task 3: Reconcile interrupted subagent lanes

**Files:**
- Modify: `crates/threadlane-agent/src/op_log.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: both files

**Interfaces:**

```rust
#[derive(Debug, Clone)]
struct InterruptedSubagentLane {
    lane: String,
    run_id: String,
    task: String,
    messages: Vec<AgentMessage>,
    safe_tools: Vec<OpRecord>,
    unsafe_tools: Vec<OpRecord>,
}

fn interrupted_subagent_lanes(records: &[OpRecord]) -> Vec<InterruptedSubagentLane>;
```

- [ ] Write `interrupted_subagent_lanes_group_deferred_messages_by_open_run`: finished runs are excluded; deferred messages retain sequence order.
- [ ] Write `unsafe_interrupted_tool_is_synthesized_once`: recovery produces one error tool message and no replay candidate.
- [ ] Run `rtk cargo test -p threadlane-agent interrupted_subagent`; expect compile failure.
- [ ] Implement grouping from existing records: open `OperationStarted` minus `OperationFinished`, matching `TaskAttempt`, ordered `WriteDeferred`, and `ToolStarted` split by replay safety.
- [ ] Deduplicate tool intents by `tool_call_id`; ignore a tool intent when deferred messages already contain its tool result.
- [ ] Run focused op-log tests and `rtk cargo test -p threadlane-agent`; expect PASS.
- [ ] Commit with `git commit -m "feat: reconcile interrupted subagent lanes"`.

---

### Task 4: Restore safe lanes and abort unsafe lanes

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs`

**Interfaces:**

```rust
async fn recover_interrupted_subagent_lanes(&mut self) -> Result<usize, String>;
```

- [ ] Write `safe_subagent_recovery_replays_tool_once`: seed start/task/deferred/safe-tool records, recover twice, assert one tool result, one finished record, and unchanged second recovery.
- [ ] Write `unsafe_subagent_recovery_aborts_without_execution`: seed an unsafe `run_command`, recover, assert executor count zero, one synthetic error result, and `OperationFinished::Aborted`.
- [ ] Run both tests; expect compile failure because recovery is absent.
- [ ] During `CodingAgent::new`, load the journal but defer async recovery until the first `handle_input_with_images` call.
- [ ] Materialize each lane marker plus deferred messages with `append_passive_branch`.
- [ ] For safe intents, call the existing `replay_safe_tools`, append returned tool messages, checkpoint them, and finish `Completed` or `Failed`.
- [ ] For unsafe intents, append one synthetic error tool message, checkpoint it, and finish `Aborted`; never call an executor.
- [ ] Before appending, detect existing `subagent_lane` markers by `run_id`; skip already materialized lanes.
- [ ] Run focused recovery tests plus `rtk cargo test -p threadlane-coding-agent subagent`; expect PASS.
- [ ] Commit with `git commit -m "feat: recover interrupted subagent lanes"`.

---

### Task 5: Cancellation, validation, and convention

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs`
- Modify: `AGENTS.md`
- Test: coding-agent and supervisor tests

**Interfaces:**
- Existing parent cancellation must journal child `OperationFinished::Aborted` before aborting its task handle.

- [ ] Write `cancelling_parent_aborts_open_subagent_operations` and assert all open child run IDs receive one aborted finish.
- [ ] Run the focused test; expect failure because cancellation only changes supervisor status.
- [ ] Add a journal helper that finishes every open run for the parent session, and call it from cancellation before aborting the runtime handle.
- [ ] Update `AGENTS.md`: child intent is durable before model/tool work; checkpoints use `WriteDeferred`; safe replay is automatic and unsafe interruption aborts.
- [ ] Run:

```bash
rtk cargo test -p threadlane-agent
rtk cargo test -p threadlane-coding-agent subagent
rtk cargo test -p threadlane-coding-agent supervisor::tests
rtk cargo check -p threadlane
rtk git diff --check
```

- [ ] Review that no unsafe executor is called during recovery, no second restore duplicates records/nodes, and ordinary chat without a persisted session remains unchanged.
- [ ] Commit with `git commit -m "docs: record in-flight subagent recovery"`.
