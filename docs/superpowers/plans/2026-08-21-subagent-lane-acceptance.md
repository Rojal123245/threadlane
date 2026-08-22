# Subagent Lane Acceptance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Execute subagents on their already-created durable child lanes and report an error only when a batch produces no successful child result.

**Architecture:** `CodingSessionHarness::start_subagent_lane` remains the sole creator of a child operation and returns an `AcceptedRun` proof alongside its lane identity. `run_subagent_task` consumes that token directly; aggregation inspects all child results after execution and turns only all-failed batches into `Err`.

**Tech Stack:** Rust, Tokio, Threadlane harness V2 JSONL store, existing `threadlane-session` unit tests.

## Global Constraints

- Keep the parent session JSONL as the canonical store for child lineage and progress.
- Do not add dependencies or separate child session files.
- Preserve recovery, cancellation, checkpointing, parallel, sequential, and lifecycle behavior.
- Mixed-success batches remain successful with explicit per-child outcomes.
- All-child failure returns an error containing per-child context.
- Follow red-green-refactor and run `cargo check -p threadlane-gpui` plus `git diff --check`.

---

## File Structure

- Modify `crates/threadlane-session/src/coding_agent/harness.rs`: expose a start result containing `SubagentLaneIdentity` and the validated `AcceptedRun` for the operation that was just committed.
- Modify `crates/threadlane-session/src/coding_agent/subagents.rs`: carry the accepted token into child execution, remove foreground `begin_run_text`, and enforce aggregate failure semantics.
- Modify colocated `#[cfg(test)]` modules in the same subsystem files: add lane acceptance and aggregation regressions using real harness state and the existing test execution hooks.
- Modify `AGENTS.md` only if implementation reveals a new durable repository convention not already documented.

### Task 1: Dedicated Child Lane Acceptance

**Files:**
- Modify: `crates/threadlane-session/src/coding_agent/harness.rs`
- Modify: `crates/threadlane-session/src/coding_agent/subagents.rs`
- Test: colocated tests in `crates/threadlane-session/src/coding_agent/harness.rs` and/or `crates/threadlane-session/src/coding_agent/subagents.rs`

**Interfaces:**
- Consumes: `CodingSessionHarness::start_subagent_lane(lane_hint, task, source_leaf_id)` and `AgentRuntime::run_accepted(run_id, lane, accepted_through_seq)`.
- Produces: a start result containing `identity: SubagentLaneIdentity` and `accepted: AcceptedRun`, where both values name the same generated child run and lane.

- [ ] **Step 1: Write the failing regression test**

Create a real temporary JSONL harness, begin a parent run on `main`, start a child lane, and assert the returned accepted token identifies the child lane/run and validates while `main` remains occupied. Exercise the child startup helper far enough to prove it no longer calls foreground acceptance.

- [ ] **Step 2: Run the focused test and verify RED**

Run the exact new test with:

```bash
cargo test -p threadlane-session <new_test_name> -- --nocapture
```

Expected before implementation: compilation failure because `start_subagent_lane` does not expose an `AcceptedRun`, or runtime failure containing `lane main is busy` when the existing startup path is exercised.

- [ ] **Step 3: Implement the minimal lifecycle correction**

After `start_subagent_lane` durably commits the child operation, prompt entry, assistant attempt, and lifecycle record, construct the `AcceptedRun` from those committed identifiers and the maximum committed sequence. Validate it against the harness before returning it with the identity. Change `run_subagents_with_context` and `run_subagent_task` to pass and consume this token. Remove the `begin_run_text(prompt_text)` call for normal child execution; recovery must continue the existing child identity without opening `main`.

- [ ] **Step 4: Run focused lane and lifecycle tests and verify GREEN**

```bash
cargo test -p threadlane-session <new_test_name> -- --nocapture
cargo test -p threadlane-session coding_agent -- --nocapture
```

Expected: the regression passes and existing child lifecycle, recovery, cancellation, parallel, and sequential tests remain green.

- [ ] **Step 5: Review the change**

Confirm exactly one operation and one prompt are created for each new child, no child startup accepts on `main`, and failed children still reach existing durable finalization.

### Task 2: Aggregate Failure Semantics

**Files:**
- Modify: `crates/threadlane-session/src/coding_agent/subagents.rs`
- Test: colocated tests in `crates/threadlane-session/src/coding_agent/subagents.rs`

**Interfaces:**
- Consumes: `Vec<Result<SubagentResult, String>>` and corresponding `Vec<CompletedSubagentLane>` produced by `run_subagents_with_context`.
- Produces: `Ok((formatted_output, thinking, lanes))` when at least one child succeeds; `Err(String)` with each child failure context when no child succeeds.

- [ ] **Step 1: Write two failing aggregation tests**

Add one test where every child result fails and assert the aggregation returns `Err` containing each agent/task failure. Add one mixed test and assert aggregation returns `Ok`, includes both explicit statuses, and preserves the successful output.

- [ ] **Step 2: Run both tests and verify RED**

```bash
cargo test -p threadlane-session <all_failed_test_name> -- --nocapture
cargo test -p threadlane-session <mixed_outcome_test_name> -- --nocapture
```

Expected before implementation: the all-failed case incorrectly returns `Ok`; the mixed case documents and protects the existing successful behavior.

- [ ] **Step 3: Implement minimal aggregation logic**

Extract or update the aggregation boundary after all child executions. Define child success consistently with durable lane status: an `Ok(SubagentResult)` with no embedded error. If none succeed, return an error built from the same per-child formatted details. Otherwise return the existing formatted aggregate unchanged so mixed batches retain every status.

- [ ] **Step 4: Run focused tests and verify GREEN**

```bash
cargo test -p threadlane-session <all_failed_test_name> -- --nocapture
cargo test -p threadlane-session <mixed_outcome_test_name> -- --nocapture
cargo test -p threadlane-session coding_agent -- --nocapture
```

Expected: both new semantics pass and existing subagent tests remain green.

- [ ] **Step 5: Review the change**

Confirm empty input remains governed by existing validation, infrastructure errors still fail immediately, mixed batches are not promoted to errors, and all-failed errors retain actionable child context.

### Task 3: Full Verification

**Files:**
- Review: all modified files
- Modify: `AGENTS.md` only if a genuinely new durable convention was discovered

**Interfaces:**
- Consumes: completed Tasks 1 and 2.
- Produces: a buildable GPUI dependency graph and whitespace-clean patch matching the approved design.

- [ ] **Step 1: Run the full session crate tests**

```bash
cargo test -p threadlane-session
```

Expected: PASS.

- [ ] **Step 2: Run the required GPUI check**

```bash
cargo check -p threadlane-gpui
```

Expected: PASS; unrelated pre-existing warnings may remain but no errors are allowed.

- [ ] **Step 3: Check patch whitespace**

```bash
git diff --check
```

Expected: no output and exit status 0.

- [ ] **Step 4: Review the final diff against the design**

Verify no separate session store, dependency, role-to-lane coupling, duplicate prompt, duplicate child operation, or unrelated refactor was introduced. Verify tests cover dedicated acceptance, no `main` acceptance, all-failed error, and mixed success.

- [ ] **Step 5: Commit the implementation**

```bash
git add crates/threadlane-session/src/coding_agent/harness.rs crates/threadlane-session/src/coding_agent/subagents.rs AGENTS.md
git commit -m "fix: run subagents on dedicated harness lanes"
```

Include `AGENTS.md` only if it changed.
