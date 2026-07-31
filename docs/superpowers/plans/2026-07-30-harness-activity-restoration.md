# Harness Activity Restoration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the latest 20 durable harness/subagent activity summaries when a session is reopened, including cancelled and completed work.

**Architecture:** Reuse the session's existing `*.oplog.jsonl`. A pure reducer in the chat state module converts `threadlane_agent::OpRecord` values into `HarnessActivity` rows grouped by durable `run_id`; the application calls it while restoring a session and seeds `ChatData` before rendering. Existing live `AgentEvent` reduction remains the update path after startup.

**Tech Stack:** Rust, Makepad UI state, `threadlane-agent` oplog records, existing session restore flow, Rust unit tests.

## Global Constraints

- Do not add a second persistence file or change the oplog schema.
- Keep the latest 20 activities per session, including terminal cancelled/completed rows.
- Treat malformed or missing oplog data as an empty activity list and never fail session startup.
- Preserve the existing durable activity keys and live event reducer behavior.
- Keep changes focused; do not alter rail layout or introduce a new UI component.

---

### Task 1: Reconstruct activities from persisted operation records

**Files:**
- Modify: `crates/threadlane/src/panels/chat/state.rs`
- Test: `crates/threadlane/src/panels/chat/state.rs` module tests

**Interfaces:**
- Consumes: `&[threadlane_agent::OpRecord]` and the existing `HarnessActivity` types.
- Produces: `pub fn harness_activities_from_oplog(records: &[threadlane_agent::OpRecord]) -> Vec<HarnessActivity>`.

- [ ] **Step 1: Add failing reducer tests**

Add record constructors in the existing test module and cover completed, cancelled, unfinished, empty, and over-cap histories. The core assertion should be:

```rust
let activities = harness_activities_from_oplog(&[
    subagent_started("run-complete", 1),
    task_attempt("run-complete", "inspect source", 2),
    operation_finished("run-complete", OpOutcome::Completed, 3),
    subagent_started("run-cancelled", 4),
    task_attempt("run-cancelled", "inspect tests", 5),
    operation_finished("run-cancelled", OpOutcome::Aborted, 6),
]);

assert_eq!(activities[0].key, "run-cancelled");
assert_eq!(activities[0].status, HarnessActivityStatus::Cancelled);
assert_eq!(activities[1].status, HarnessActivityStatus::Recovered);
```

For 21 occurrences, assert the result length is 20 and the newest key is first. Assert an open occurrence is retained as unresolved and a record without a task attempt is skipped.

- [ ] **Step 2: Run the focused tests and verify failure**

Run `cargo test -p threadlane --bin threadlane panels::chat::state::tests::restores_completed_and_cancelled_subagents`.

Expected: compilation or test failure because `harness_activities_from_oplog` does not exist yet.

- [ ] **Step 3: Implement the minimal reducer**

Process records in `(seq, original_index)` order. For each `OperationStarted` with `kind == "subagent"`, create an occurrence keyed by `(lane, run_id)`. Attach the latest `TaskAttempt` task. On `OperationFinished`, map `Completed` to `Recovered`, `Aborted` to `Cancelled`, `Failed` to `Aborted`, and `Declined` to `Aborted`; retain the optional error as detail. Leave an open occurrence as `Working` with detail `"Restored from session history"`.

Use the durable run ID as `HarnessActivity.key`, the existing subagent identity fallback for `agent`, sort by each occurrence's latest sequence descending, and return only the first 20. Do not add a record type or write to disk.

- [ ] **Step 4: Run all chat state tests**

Run `cargo test -p threadlane --bin threadlane panels::chat` and expect all chat tests to pass.

- [ ] **Step 5: Commit the reducer**

```bash
git add crates/threadlane/src/panels/chat/state.rs
git commit -m "feat: restore harness activities from oplog"
```

### Task 2: Seed restored activities during session restore

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/workspace/mod.rs` only if the existing constructor needs a restore parameter
- Test: `crates/threadlane/src/app/mod.rs` or the existing chat state test module

**Interfaces:**
- Consumes: `threadlane_agent::load_op_records_from_file`, the session file path already used by restore, and `harness_activities_from_oplog`.
- Produces: a restored `ChatData.harness_activities` vector before the session's first draw.

- [ ] **Step 1: Locate the canonical session creation/switch path**

Trace the existing `SessionWorkspace::default`/`SessionWorkspace::new` call and the code that loads the session JSONL/oplog. Use that same path rather than adding a second startup hook. Confirm that opening a different session clears or replaces the previous workspace’s activities.

- [ ] **Step 2: Add a failing restore-path assertion**

Build a temporary oplog containing one completed and one aborted subagent, invoke the existing restore seam, and assert the resulting workspace contains both durable keys and terminal statuses. If the app path needs Makepad context, test the smallest helper that loads the optional oplog and assigns the vector; keep the reducer fully covered in Task 1.

- [ ] **Step 3: Wire optional oplog loading**

Load `session_file.with_extension("oplog.jsonl")` with the existing tolerant loader. On any I/O error, use `Vec::new()`. Assign:

```rust
workspace.chat.harness_activities = harness_activities_from_oplog(&records);
```

Run this after the workspace is created and before redraw/session-list synchronization. Keep normal live event handling unchanged so matching events replace restored rows by key.

- [ ] **Step 4: Verify session switching does not leak rows**

Run the focused restore test and assert that a session with no oplog starts with zero activities after switching from a session with restored rows.

- [ ] **Step 5: Commit the restore wiring**

```bash
git add crates/threadlane/src/app/mod.rs crates/threadlane/src/workspace/mod.rs
git commit -m "feat: seed harness history on session restore"
```

### Task 3: Verify the complete slice

**Files:**
- No new source files.

- [ ] **Step 1: Run focused tests**

```bash
cargo test -p threadlane --bin threadlane panels::chat
cargo test -p threadlane --bin threadlane panels::sessions
```

Expected: all tests pass.

- [ ] **Step 2: Run compile and whitespace checks**

```bash
cargo check -p threadlane
git diff --check HEAD~2..HEAD
```

Expected: `cargo check` succeeds and `git diff --check` prints no errors.

- [ ] **Step 3: Inspect the final diff**

```bash
git status --short
git log -3 --oneline
```

Confirm only the reducer, restore wiring, tests, and approved documentation are present. Visual runtime verification remains separate because the current computer-use target resolves to the packaged stale app rather than the development binary.

