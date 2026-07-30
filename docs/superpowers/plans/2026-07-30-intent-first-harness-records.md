# Complete Intent-First Harness Records Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans (recommended) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `OperationStarted`, `TaskAttempt`, `ToolStarted`, and `QueueEnqueued` durable intent records with IDs allocated and persisted before the associated work executes.

**Architecture:** Keep the existing JSONL `OpRecord` schema and `HarnessSupervisor` lane ownership. `OperationStarted` and `TaskAttempt` stay in `submit_input`; tool intents move from the asynchronous event observer into a synchronous pre-execution recorder supplied to `AgentLoop`. Queue APIs persist `QueueEnqueued` through the lane’s session file before mutating the in-memory queue. Existing `ToolExecutionStart` events remain observational only and must not create duplicate records.

**Tech Stack:** Rust, Tokio, serde JSONL operation logs, existing `AgentLoop` tool hooks, `HarnessSupervisor` lanes, Cargo tests.

## Global Constraints

- Use the existing `OpRecord` variants and JSONL append/locking helpers; do not add a new persistence backend.
- Allocate IDs and sequence numbers before executing the tool, LLM call, or queue mutation.
- Preserve current `OperationStarted` and `TaskAttempt` behavior.
- A failed intent append must prevent the associated action from starting or entering the queue.
- Keep ordinary chat runtime behavior unchanged unless it is explicitly using a harness lane.
- Use TDD for each behavioral change: failing test, confirm red, minimal implementation, focused green test.
- Do not edit generated content under `target/`, `crates/threadlane/dist/`, or deployed `.threadlane/` artifacts.

---

### Task 1: Establish a synchronous tool-intent recorder contract

**Files:**
- Modify: `crates/threadlane-agent/src/loop_engine.rs: ToolRunContext, AgentLoop, execute_tools, run_tool_with_hooks`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs: CodingAgent construction/configuration`
- Test: `crates/threadlane-agent/src/loop_engine.rs` tests and `crates/threadlane-coding-agent/src/coding_agent.rs` tests

**Interfaces:**
- Produce an optional `AgentLoop` recorder callback invoked after tool arguments are normalized and before `ToolExecutionStart` is emitted or any executor is called.
- The callback receives the normalized tool call ID, name, and effective arguments and returns `Result<(), String>`.
- The existing event stream remains available for UI observation but is no longer the persistence source for `ToolStarted`.

- [ ] **Step 1: Write the failing test**

Add a test that installs a recorder, executes a deterministic read-only tool, and asserts the recorder sees the call before the tool result is produced. Add a failure-path test where the recorder returns an error and assert the executor is not invoked and the returned `AgentToolResult` is an error.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:
```bash
rtk cargo test -p threadlane-agent tool_intent_recorder
```

Expected: compile/test failure because no recorder API exists.

- [ ] **Step 3: Implement the minimal callback plumbing**

Add the recorder to `AgentLoop`, copy it into sequential and parallel `ToolRunContext` values, and invoke it in `run_tool_with_hooks` immediately before execution. Convert recorder failure into a normal tool error without invoking the executor.

- [ ] **Step 4: Run the focused tests**

Run:
```bash
rtk cargo test -p threadlane-agent tool_intent_recorder
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/threadlane-agent/src/loop_engine.rs crates/threadlane-coding-agent/src/coding_agent.rs
git commit -m "feat: add synchronous tool intent recorder"
```

---

### Task 2: Persist `ToolStarted` before tool execution

**Files:**
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs: append_tool_started_record, create_task event wiring, submit_input`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs: expose recorder configuration for the active run`
- Modify: `crates/threadlane-agent/src/op_log.rs: ID/sequence helpers only if needed`
- Test: `crates/threadlane-coding-agent/src/supervisor.rs`

**Interfaces:**
- Add a per-run recorder configuration that captures `session_id`, lane name, session file, and active run ID.
- The recorder must allocate the `ToolStarted.id` and sequence while reserving the lane record, append it to the oplog, then allow execution to continue.
- `append_tool_started_record` remains the single construction path for the record.
- Remove persistence from the asynchronous `AgentEvent::ToolExecutionStart` listener; retain event forwarding for UI/activity state.

- [ ] **Step 1: Write the failing ordering test**

Add a supervisor test that invokes a tool through a configured active run and observes the oplog from inside the tool executor. Assert the `ToolStarted` record already exists and its ID is non-empty before the executor returns.

- [ ] **Step 2: Run the test and verify it fails**

Run:
```bash
rtk cargo test -p threadlane-coding-agent tool_started_is_durable_before_execution
```

Expected: FAIL because the current asynchronous event listener has not persisted the record when execution begins.

- [ ] **Step 3: Implement synchronous persistence**

Configure the recorder immediately after `OperationStarted`/`TaskAttempt` are persisted and before `handle_input` begins. Make lane sequence reservation and JSONL append atomic with respect to concurrent tool calls so parallel execution cannot reuse a sequence. Do not write a second record from `ToolExecutionStart`.

- [ ] **Step 4: Add a recorder-failure regression**

Assert that a failed oplog append yields a tool error and the executor does not run.

- [ ] **Step 5: Run focused and regression tests**

Run:
```bash
rtk cargo test -p threadlane-coding-agent tool_started_is_durable_before_execution
rtk cargo test -p threadlane-agent
rtk cargo test -p threadlane-coding-agent supervisor::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-agent/src/op_log.rs crates/threadlane-coding-agent/src/coding_agent.rs crates/threadlane-coding-agent/src/supervisor.rs
git commit -m "feat: persist tool intents before execution"
```

---

### Task 3: Persist queue intents before enqueueing

**Files:**
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs: Lane, enqueue_steer, enqueue_steer_priority, enqueue_followup`
- Modify: `crates/threadlane-agent/src/op_log.rs: QueueEnqueued helpers/tests only if needed`
- Test: `crates/threadlane-coding-agent/src/supervisor.rs`

**Interfaces:**
- Ensure each harness lane knows its session file once it is created for a persisted task.
- Add one internal helper that allocates the queue record ID/sequence and persists:
```rust
fn append_queue_enqueued_record(
    &self,
    session_id: &str,
    lane_name: &str,
    kind: QueueKind,
    target: AgentMessage,
) -> Result<(), String>
```
- The three queue methods must persist `QueueEnqueued` before mutating `LaneQueue`.
- If no session file is configured, return an explicit error for persisted harness lanes rather than silently claiming durability.

- [ ] **Step 1: Write failing tests**

Add tests for steer, priority steer, and follow-up enqueue that assert:
1. the JSONL record exists before the queue contains the message;
2. the record has the correct `QueueKind`, target, lane, and non-empty ID;
3. a persistence failure leaves the in-memory queue unchanged.

- [ ] **Step 2: Run the tests and verify they fail**

Run:
```bash
rtk cargo test -p threadlane-coding-agent queue_enqueued_is_persisted_before_mutation
```

Expected: FAIL because current queue methods only mutate memory.

- [ ] **Step 3: Implement lane session-file tracking and the helper**

Set the lane session file from the existing persisted operation path. Use the existing file lock/append helper and reserve sequence numbers under the lane lock so concurrent enqueue calls cannot collide.

- [ ] **Step 4: Wire all queue variants**

Call the helper from `enqueue_steer`, `enqueue_steer_priority`, and `enqueue_followup`; only call the queue mutation after persistence succeeds. Preserve existing priority ordering.

- [ ] **Step 5: Run focused tests**

Run:
```bash
rtk cargo test -p threadlane-coding-agent queue_enqueued_is_persisted_before_mutation
rtk cargo test -p threadlane-coding-agent supervisor::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-coding-agent/src/supervisor.rs crates/threadlane-agent/src/op_log.rs
git commit -m "feat: persist queue enqueue intents"
```

---

### Task 4: Verify recovery and duplicate prevention

**Files:**
- Modify: `crates/threadlane-agent/src/op_log.rs: reconcile_op_log_recovery` only if queue/tool intent handling needs it
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs: restore_session_lanes` only if queue replay/reconciliation needs it
- Test: `crates/threadlane-agent/src/op_log.rs`
- Test: `crates/threadlane-coding-agent/src/supervisor.rs`

**Interfaces:**
- Reopening a session with a durable `ToolStarted` must recover it exactly once and never duplicate the intent.
- Reopening a session with `QueueEnqueued` must restore the queued target exactly once, or explicitly leave it pending according to existing queue semantics; do not execute it twice.
- Completed/aborted/failed operations must remove their IDs from the open-operation set.

- [ ] **Step 1: Write restart/reconciliation tests**

Cover:
1. a persisted tool intent with no result produces one recovery action;
2. a second restore produces no second tool intent/result;
3. a persisted queue intent survives restore and is not duplicated;
4. an aborted operation does not reappear as open.

- [ ] **Step 2: Run tests and verify missing behavior**

Run:
```bash
rtk cargo test -p threadlane-agent op_log_recovery
rtk cargo test -p threadlane-coding-agent supervisor::tests
```

Expected: FAIL only for newly required queue restoration or duplicate-prevention behavior.

- [ ] **Step 3: Implement the smallest reconciliation changes**

Reuse existing `RecoveryResult`, lane grouping, and synthetic tool-result logic. Add only the queue restoration bookkeeping required by the tests; do not introduce a second queue abstraction.

- [ ] **Step 4: Run all focused recovery tests**

Run:
```bash
rtk cargo test -p threadlane-agent
rtk cargo test -p threadlane-coding-agent supervisor::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/threadlane-agent/src/op_log.rs crates/threadlane-coding-agent/src/supervisor.rs
git commit -m "test: verify intent recovery is idempotent"
```

---

### Task 5: Final validation and documentation

**Files:**
- Modify: `AGENTS.md` only if implementation reveals a durable repository convention
- No generated files

- [ ] **Step 1: Run the required validation**

```bash
rtk cargo test -p threadlane-agent
rtk cargo test -p threadlane-coding-agent supervisor::tests
rtk cargo check -p threadlane
rtk git diff --check
```

Document any existing sandbox-only network test failures without changing unrelated tests.

- [ ] **Step 2: Review the diff**

Confirm:
- no asynchronous event listener remains responsible for durable `ToolStarted`;
- no queue mutation happens before `QueueEnqueued` persistence;
- IDs and sequence numbers are allocated before execution/mutation;
- ordinary chat behavior is unchanged;
- no duplicate records are emitted.

- [ ] **Step 3: Commit final documentation/convention changes if needed**

```bash
git add AGENTS.md
git commit -m "docs: record intent-first harness convention"
```

