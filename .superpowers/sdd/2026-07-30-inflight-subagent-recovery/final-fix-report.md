# In-Flight Subagent Recovery — Final Fix Report

Date: 2026-07-30

## Scope

This wave resolves the complete final-review ownership list for durable
in-flight model-subagent recovery. It uses the existing session JSONL and
sidecar operation journal only; no persistence backend or dependency was
added.

## Architecture and fixes

### Durable, occurrence-safe child identities

- A subagent start now reserves its identity while holding the shared
  per-session journal mutex.
- The first persisted `OperationStarted` sequence becomes the durable
  occurrence identity (`subagent-run-<seq>`), and the lane name is suffixed
  with the same sequence.
- Sequence allocation uses the maximum persisted sequence rather than a
  process-local counter or snapshot length.
- Terminal matching resolves the currently open lane/run occurrence instead
  of suppressing a new occurrence because an old terminal reused an ID.
- Recovery markers match the `(lane, run_id)` pair.

The process-local numeric ID remains only for transient UI event correlation;
it is no longer a persistence identity.

### Passive branch durability before terminalization

- Completed child results enter the completed-lane sink without writing a
  terminal record.
- `commit_completed_subagent_lanes` durably appends the passive branch first,
  preserving the parent active leaf, and only then appends
  `OperationFinished`.
- If branch persistence fails, the uncommitted lane and all following lanes
  return to the sink.
- If terminal persistence fails after the branch is durable, the current lane
  is not duplicated. Recovery remains pending and recognizes the durable
  marker, checkpoints any branch messages not yet in the sidecar, and writes
  the single terminal.

### Actual child continuation

- Open child occurrences retain their durable task, checkpoint messages,
  start sequence, and source parent leaf.
- When no unsafe tool remains open, recovery creates a short-lived child agent,
  seeds it with the latest durable checkpoint, and requests another model turn
  with: `Continue from the recovered checkpoint and finish the assigned task.`
- A safely replayed tool result is checkpointed and included in that resumed
  provider history before continuation.
- Checkpoint cursors begin after seeded recovery history so old messages are
  not duplicated.
- Unsafe interrupted tools are never executed and the child occurrence is
  materialized and terminalized as aborted.

### Crash-idempotent safe replay and recorder isolation

- Before executing a safe replay, recovery appends a durable replay claim
  under the shared journal lock.
- The claim reuses the existing `ToolStarted` record shape with replay safety
  set to `Never`. This deliberately avoids a schema migration: after a crash,
  the claimed call is unreplayable and is aborted rather than executed twice.
- A successful replay checkpoints its real tool result before child
  continuation.
- `AgentLoop` exposes a replay execution path that retains normal hooks and
  executors but omits the live intent recorder, so supervisor recovery cannot
  record child replay evidence under the `main` lane.

### One shared per-session journal

- A process-wide weak registry returns one journal state mutex for each
  sidecar path.
- `CodingAgent` captures that journal once and shares it through every
  `AgentRunner` invocation.
- Start, tool intent, checkpoint, replay claim, terminal, and cancellation
  appends allocate sequences and write while holding the same child-journal
  mutex.

### Cancellation coordination

- Cancellation enters a counted journal guard, appends aborted terminals for
  every open child while holding the same lock used by child start and
  terminalization, and rejects starts, tool intents, and checkpoints while the
  guard is active.
- A racing completed terminal becomes a no-op after the cancellation terminal,
  preserving one terminal per occurrence.
- The supervisor stores the guard in the task runtime after requesting abort.
  It remains installed until a later submission has acquired the serialized
  prompt lock, ensuring the cancelled runtime cannot start a late child in the
  abort window.

### Parent lineage

- Child `OperationStarted` records persist the originating parent leaf.
- Interrupted occurrence reconstruction carries that leaf into recovery.
- Recovered and normally completed passive branches append from the originating
  parent, while the parent active leaf remains unchanged.

## Regression coverage

Production-path regressions cover:

- durable identity allocation across a fresh journal reload;
- concurrent starts sharing one monotonic sequence allocator;
- occurrence-safe finish behavior across separately loaded journal handles;
- passive branch persistence before terminalization;
- a simulated crash after passive-branch persistence and before terminal
  persistence;
- deterministic model continuation from the latest child checkpoint;
- safe replay result inclusion in resumed child context;
- durable replay claims surviving a fresh restore;
- replay bypassing the normal live intent recorder;
- safe replay occurring at most once across repeated and fresh restores;
- unsafe tools never executing;
- cancellation rejecting a racing child start and emitting one aborted
  terminal;
- supervisor cancellation retaining its guard until the next serialized
  submission;
- recovered branch parent lineage and unchanged active parent leaf.

Recovery tests use `SubagentLaneJournal::start`, `tool_started`, `checkpoint`,
the real session sidecar, and real `SessionTree` persistence. The former
manual test helper that injected fabricated operation records directly into
the sidecar was removed.

## Verification

- `rtk cargo test -p threadlane-agent` — 69 passed.
- `rtk cargo test -p threadlane-coding-agent` — 157 passed.
- `rtk cargo test -p threadlane-coding-agent subagent` — 29 passed.
- `rtk cargo test -p threadlane-coding-agent supervisor::tests` — 18 passed.
- `rtk cargo check -p threadlane` — passed with no errors.
- `rtk git diff --check` — passed.

The desktop check continues to report the repository's existing duplicate
Makepad-package notices and the pre-existing unused
`reset_title_attempt` warning.

## Files in this fix wave

- `crates/threadlane-agent/src/loop_engine.rs`
- `crates/threadlane-agent/src/op_log.rs`
- `crates/threadlane-coding-agent/src/coding_agent.rs`
- `crates/threadlane-coding-agent/src/supervisor.rs`
- `.superpowers/sdd/2026-07-30-inflight-subagent-recovery/final-fix-report.md`

All unrelated dirty files remain untouched. Pre-existing unstaged formatting
edits in `op_log.rs` and `supervisor.rs` are intentionally excluded from the
fix-wave index and commit.
