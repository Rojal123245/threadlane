# Canonical Threadlane Agent Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route foreground chat, explicit `/task`, and model subagents through one durable harness while removing duplicate persistence and lane orchestration.

**Architecture:** Keep `threadlane-agent::harness` as the durable authority. Add a value-based session/lane API around the existing `AgentHarness`, then migrate `CodingAgent` and `HarnessSupervisor` to that API. `UnifiedAgent` and `AgentLoop` remain compatibility facades while production callers stop mutating their own persistence paths. ACP remains outside this migration.

**Tech Stack:** Rust 2021, Tokio, Serde/JSONL/SQLite stores, existing `AgentHarness`, `Reducer`, `GatedEffects`, `HarnessEventHub`, `TurnDriver`, `CodingAgent`, and `HarnessSupervisor`.

---

## File map and ownership

| File | Responsibility after this plan |
|---|---|
| `crates/threadlane-agent/src/harness/session.rs` | Value-based `LaneHandle` and `SessionAgent<S>` facade over one `AgentHarness<S>` |
| `crates/threadlane-agent/src/harness/mod.rs` | Register and export the session facade without leaking unrelated implementation details |
| `crates/threadlane-agent/tests/harness_session.rs` | Memory/JSONL contract tests for lane acceptance, snapshots, events, and sequence uniqueness |
| `crates/threadlane-agent/src/unified.rs` | Compatibility runtime and provider/tool composition; no direct production queue/persistence writes outside `SessionAgent` |
| `crates/threadlane-agent/src/turn_driver.rs` | Provider/tool execution adapter; durable writes enter through the session facade |
| `crates/threadlane-coding-agent/src/coding_agent/harness.rs` | Coding-agent-owned adapter that constructs and exposes one canonical session harness |
| `crates/threadlane-coding-agent/src/coding_agent/mod.rs` | Public `CodingAgent` composition and behavior; remove duplicate main-session journal/lifecycle writes |
| `crates/threadlane-coding-agent/src/supervisor.rs` | Explicit `/task` registry/scheduler and task projection only |
| `crates/threadlane-coding-agent/src/coding_agent/subagents.rs` | Deterministic child lane setup, result projection, and recovery after convergence |
| `crates/threadlane-coding-agent/src/coding_agent/harness_journal.rs` | Transitional compatibility adapter; delete after all callers migrate |
| `crates/threadlane-agent/src/events.rs` and `crates/threadlane-agent/src/harness/events.rs` | One canonical durable event projection and compatibility conversions |
| `harness_v2.md`, `docs/harness-v2-format.md`, `README.md` | Final ownership, migration, and public architecture documentation |

## Global constraints

- Do not change ACP transport behavior or make ACP pretend to be V2 durable.
- Do not add a second persistence format, sequence allocator, reducer, or supervisor-side recovery model.
- Preserve legacy JSONL transcript/configuration loading and the public `AgentLoop` compatibility facade.
- Do not edit generated `target/`, `crates/threadlane/dist/`, or deployed `.threadlane/` content.
- Subagents must edit only assigned files and must skip formatters, linters, builds, and project-wide tests. The orchestrator runs gates at phase boundaries.
- Every accepted operation must persist intent before provider/tool effects; every completion event must follow the durable commit.

---

## Phase 1 — Canonical session and lane surface

### Task 1: Add value-based session and lane handles

**Files:**
- Create: `crates/threadlane-agent/src/harness/session.rs`
- Modify: `crates/threadlane-agent/src/harness/mod.rs`
- Test: `crates/threadlane-agent/tests/harness_session.rs`

- [ ] **Step 1: Define the public value types.**

Add:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaneHandle {
    name: String,
}

pub struct SessionAgent<S: SessionStore> {
    harness: AgentHarness<S>,
}
```

`LaneHandle::new` rejects an empty or whitespace-only name with `ReduceError::InvalidLane`. Expose `name(&self) -> &str`. `SessionAgent::new` accepts an existing `AgentHarness<S>`; `harness` and `harness_mut` remain available only to the coding-agent adapter during migration.

- [ ] **Step 2: Add canonical lane operations.**

Implement methods that delegate to existing `AgentHarness` procedures and drive only the selected lane:

```rust
impl<S: SessionStore> SessionAgent<S> {
    pub fn lane(&self, name: &str) -> Result<LaneHandle, ReduceError>;
    pub fn main_lane(&self) -> Result<LaneHandle, ReduceError>;
    pub fn snapshot(&self) -> Result<Snapshot, ReduceError>;
    pub fn watch(&self, lane: &LaneHandle) -> Result<Subscription, ReduceError>;
    pub fn watch_session(&self) -> Result<Subscription, ReduceError>;
    pub fn accept_prompt(&mut self, lane: &LaneHandle, run_id: &str, prompt: AgentMessage) -> Result<String, ProcedureError>;
    pub fn enqueue(&mut self, lane: &LaneHandle, run_id: Option<&str>, queue: QueueKind, target: ProvisionedEntry) -> Result<(), ProcedureError>;
    pub fn cancel_queued(&mut self, lane: &LaneHandle, run_id: Option<&str>, entry_id: &str) -> Result<(), ProcedureError>;
    pub fn request_abort(&mut self, lane: &LaneHandle, run_id: &str) -> Result<(), ProcedureError>;
    pub fn drive_one(&mut self, lane: &LaneHandle) -> Result<bool, EffectsError>;
    pub fn drive_to_completion(&mut self, lane: &LaneHandle) -> Result<(), EffectsError>;
}
```

Dispatch bound/unbound queue calls according to `run_id`; do not recompute sequence numbers or construct records in this facade. Return `ReduceError::InvalidLane` for a stale handle.

- [ ] **Step 3: Register the module and write behavior tests.**

Add `pub mod session;` and exports from `harness/mod.rs`. Add tests proving:

1. `main_lane()` returns the persisted `main` lane.
2. An unknown lane is rejected without a write.
3. `accept_prompt` followed by `drive_to_completion` produces one durable run and committed event sequence.
4. Two lanes accepting prompts receive unique monotonically increasing store sequences.
5. A snapshot and a fresh subscription agree on lane state.

- [ ] **Step 4: Commit the focused core slice.**

```bash
git add crates/threadlane-agent/src/harness/session.rs crates/threadlane-agent/src/harness/mod.rs crates/threadlane-agent/tests/harness_session.rs
git commit -m "feat: add canonical session lane facade"
```

### Task 2: Normalize durable event projection

**Files:**
- Modify: `crates/threadlane-agent/src/harness/events.rs`
- Modify: `crates/threadlane-agent/src/events.rs`
- Test: `crates/threadlane-agent/tests/harness_session.rs`

- [ ] **Step 1: Add an explicit compatibility conversion.**

Define a conversion helper from committed harness payloads to optional `AgentEvent` values. It must map only durable lifecycle facts already represented by the existing `AgentEvent` enum, preserve lane/run/turn identity in the harness event, and return `None` for payloads with no compatible legacy event. Do not emit an end event before `EntryCommitted`/`RecordCommitted` publication.

- [ ] **Step 2: Make subscription state authoritative.**

Ensure `SessionAgent::watch` returns the reduced snapshot followed by buffered events from the same `HarnessEventHub` subscription. Add a commit sequence to the compatibility event adapter so consumers can discard duplicate projections without ordering guesses.

- [ ] **Step 3: Test event ordering and duplicate resistance.**

Extend `harness_session.rs` with a test that accepts a prompt, drives one action at a time, and asserts no completion event is observable before its entry/record commit. Add a reconnect test that starts from a snapshot and receives each later event exactly once.

- [ ] **Step 4: Commit the event contract.**

```bash
git add crates/threadlane-agent/src/harness/events.rs crates/threadlane-agent/src/events.rs crates/threadlane-agent/tests/harness_session.rs
git commit -m "feat: expose committed harness event projection"
```

### Phase 1 gate

Run:

```bash
cargo test -p threadlane-agent harness_session
git diff --check
```

Expected: all focused session/event tests pass and no whitespace errors. Do not begin Phase 2 on a red tree.

---

## Phase 2 — Foreground CodingAgent cutover

### Task 3: Construct one canonical session adapter

**Files:**
- Create: `crates/threadlane-coding-agent/src/coding_agent/harness.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent/mod.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent/harness_journal.rs`
- Test: `crates/threadlane-coding-agent/tests/coding_agent_tests.rs`

- [ ] **Step 1: Add the adapter type.**

Define `CodingSessionHarness` around `SessionAgent<JsonlStore>` with the saved session path, canonical `main` handle, and one subscription. Its constructor opens the existing JSONL store through the existing `JsonlStore::open` path and returns typed errors. It owns no second `SessionTree`, sequence counter, or operation log.

- [ ] **Step 2: Route main-lane acceptance through the adapter.**

Change `CodingAgent::begin_harness_run`, prompt acceptance, queue operations, cancellation, snapshot, and watch setup to call `CodingSessionHarness`. Preserve existing public `CodingAgent` methods and Makepad-facing event types. Keep `SessionTree` only as the compatibility transcript/configuration view until the final cleanup phase.

- [ ] **Step 3: Make the journal adapter observational.**

Remove main-session writes from `HarnessJournalAdapter` and the private duplicate `HarnessJournal` path. Leave only a narrow compatibility implementation for callbacks that have no V2 equivalent, and make those callbacks return their persistence error instead of ignoring it. Main assistant/tool messages, usage, and operation finishes must be committed through the canonical session path.

- [ ] **Step 4: Add foreground regression tests.**

Cover a no-tool prompt and a queued follow-up using a temporary JSONL session. Assert that each accepted prompt creates one operation start, one assistant attempt, and one operation finish, with no duplicate records from journal callbacks. Assert reopening the same session produces the same reduced main-lane state.

- [ ] **Step 5: Commit the foreground cutover.**

```bash
git add crates/threadlane-coding-agent/src/coding_agent/harness.rs crates/threadlane-coding-agent/src/coding_agent/mod.rs crates/threadlane-coding-agent/src/coding_agent/harness_journal.rs crates/threadlane-coding-agent/tests/coding_agent_tests.rs
git commit -m "refactor: route foreground coding agent through harness"
```

### Task 4: Remove duplicate UnifiedAgent queue writes

**Files:**
- Modify: `crates/threadlane-agent/src/unified.rs`
- Modify: `crates/threadlane-agent/src/turn_driver.rs`
- Test: `crates/threadlane-agent/tests/agent_tests.rs`

- [ ] **Step 1: Replace direct queue ownership in production calls.**

Keep the existing `steering_queue` and `follow_up_queue` fields only as transient execution buffers inside `TurnDriver`. Change `UnifiedAgent::steer`, `follow_up`, and prompt entry points used by `CodingAgent` to accept/consume durable queue entries through `SessionAgent`; the driver receives a prepared queue snapshot and does not append queue records.

- [ ] **Step 2: Propagate journal errors.**

Change assistant/tool/provider journal callback sites in `TurnDriver` so a failed canonical persistence call terminates the run with the typed harness error instead of continuing after a failed durable prefix. Do not change provider streaming or tool execution semantics.

- [ ] **Step 3: Preserve compatibility tests.**

Run existing no-tool, tool, parallel-order, hook, overflow, and cancellation tests through the compatibility facade. Add one assertion that a failed journal write does not produce an `AgentEnd` success event.

- [ ] **Step 4: Commit the runtime adapter cleanup.**

```bash
git add crates/threadlane-agent/src/unified.rs crates/threadlane-agent/src/turn_driver.rs crates/threadlane-agent/tests/agent_tests.rs
git commit -m "refactor: keep UnifiedAgent queues transient"
```

### Phase 2 gate

Run:

```bash
cargo test -p threadlane-agent
cargo test -p threadlane-coding-agent
cargo check -p threadlane
git diff --check
```

Expected: package tests and the application check pass. Confirm with a scoped search that `CodingAgent` has one canonical harness field and no production main-lane direct append helper.

---

## Phase 3 — HarnessSupervisor cutover

### Task 5: Make supervisor a scheduler and projection

**Files:**
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent/harness.rs`
- Test: `crates/threadlane-coding-agent/tests/supervisor_tests.rs`

- [ ] **Step 1: Replace supervisor lane persistence with a handle reference.**

Retain task identity, project identity, parent task identity, status, cancellation handle, and UI summary fields. Remove supervisor-owned operation-log mutation, sequence recomputation, `JsonlStore` reopening, and duplicated lane queue state. Store the canonical session path plus `LaneHandle`/run identity needed to submit operations.

- [ ] **Step 2: Route task lifecycle operations through the coding-session adapter.**

Change task creation, submit, steer, follow-up, resume, cancel, deferred redemption, and navigation to call canonical session methods. A task status transition is derived from the latest harness snapshot/event; it is not written as a second operation record. Preserve explicit `/task` task registry semantics and concurrent task scheduling.

- [ ] **Step 3: Remove duplicate record helpers.**

Delete or make unreachable `persist_prompt_run`, `append_v2_lifecycle_record`, `append_persisted_lane_record`, `append_tool_started_record`, supervisor-side recovery reducers, and sequence calculators. Do not remove public APIs until all internal callsites are migrated; then remove obsolete compatibility aliases in the same slice.

- [ ] **Step 4: Test restart and cancellation behavior.**

Add/retain tests proving: task restart restores status from the harness snapshot; cancellation writes one durable abort intent before the Tokio handle is interrupted; two tasks in different lanes do not share sequence IDs; task UI events remain ordered after reconnect; and foreground sessions are not inserted into the supervisor registry.

- [ ] **Step 5: Commit the supervisor cutover.**

```bash
git add crates/threadlane-coding-agent/src/supervisor.rs crates/threadlane-coding-agent/src/coding_agent/harness.rs crates/threadlane-coding-agent/tests/supervisor_tests.rs
git commit -m "refactor: make supervisor a harness scheduler"
```

### Phase 3 gate

Run:

```bash
cargo test -p threadlane-coding-agent
cargo test -p threadlane-agent
cargo check -p threadlane
git diff --check
```

Expected: no supervisor persistence duplication and all task/supervisor tests green.

---

## Phase 4 — Subagent convergence

### Task 6: Route child lanes through the canonical session

**Files:**
- Create: `crates/threadlane-coding-agent/src/coding_agent/subagents.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent/mod.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent/harness_journal.rs`
- Test: `crates/threadlane-coding-agent/tests/coding_agent_tests.rs`

- [ ] **Step 1: Move deterministic child identity and lane setup.**

Create one child-lane configuration function deriving the lane name and child run identity from the parent session identity plus tool-call ID. Reject a reused identity when the durable lane already contains a completed result; reopen the existing lane for safe replay instead of spawning a twin.

- [ ] **Step 2: Use canonical child procedures.**

Change child prompt acceptance, checkpointing, safe tool replay, unsafe interruption, passive branch commit, and cancellation to call `SessionAgent` lane operations. Keep child result formatting and UI projection separate from durability. Preserve the existing child result contract returned to the parent tool call.

- [ ] **Step 3: Delete specialized child persistence.**

After callsites are migrated, remove `SubagentJournalAdapter`, `SubagentLaneJournal`, and any child-specific direct sidecar append/recovery path. Child cancellation must persist abort intent in its own lane and must not cancel sibling lanes unless the caller explicitly requests hierarchy cancellation.

- [ ] **Step 4: Add child crash-prefix coverage.**

Cover child acceptance, provider checkpoint, safe unfinished tool replay, unsafe tool interruption, parent result commit, repeated recovery, and sibling cancellation isolation. Assert parent-visible output is committed exactly once.

- [ ] **Step 5: Commit child convergence.**

```bash
git add crates/threadlane-coding-agent/src/coding_agent/subagents.rs crates/threadlane-coding-agent/src/coding_agent/mod.rs crates/threadlane-coding-agent/src/coding_agent/harness_journal.rs crates/threadlane-coding-agent/tests/coding_agent_tests.rs
git commit -m "refactor: converge subagents on durable lanes"
```

### Phase 4 gate

Run:

```bash
cargo test -p threadlane-agent
cargo test -p threadlane-coding-agent
cargo check -p threadlane
git diff --check
```

Expected: child recovery and parent result tests pass without specialized journal code.

---

## Phase 5 — Structural cleanup and documentation

### Task 7: Split the coding-agent monolith by ownership

**Files:**
- Create: `crates/threadlane-coding-agent/src/coding_agent/runtime.rs`
- Create: `crates/threadlane-coding-agent/src/coding_agent/capabilities.rs`
- Create: `crates/threadlane-coding-agent/src/coding_agent/broker.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent/mod.rs`
- Modify: `crates/threadlane-coding-agent/src/lib.rs`

- [ ] **Step 1: Move composition-root code into `runtime.rs`.**

Move `CodingAgent` fields, constructor wiring, credential/model/session operations, and public run/cancel/watch methods into `runtime.rs`. Keep behavior and signatures unchanged; `mod.rs` should contain module declarations, stable re-exports, and no capability implementation bodies.

- [ ] **Step 2: Move capability implementations.**

Move `SkillCapability`, `SubagentCapability`, `PlanCapability`, `WasiCapability`, `McpCapability`, and capability registration helpers into `capabilities.rs`. Preserve registration order and tool/hook deduplication semantics.

- [ ] **Step 3: Move host broker helpers.**

Move managed-process state, host capability request handling, argument validation, path resolution, content-length parsing, and broker tool executors into `broker.rs`. Keep workspace path validation delegated to the existing `threadlane_tools::validate_path_in_workspace` implementation.

- [ ] **Step 4: Narrow exports and remove obsolete compatibility paths.**

Export only the stable coding-agent surface consumed by the workspace. Remove private helper re-exports, unused aliases, and the retired `harness_journal.rs` module after a repository-wide reference search confirms no caller remains.

- [ ] **Step 5: Commit structural cleanup.**

```bash
git add crates/threadlane-coding-agent/src/coding_agent crates/threadlane-coding-agent/src/lib.rs
git commit -m "refactor: split coding agent by ownership"
```

### Task 8: Update architecture documentation

**Files:**
- Modify: `harness_v2.md`
- Modify: `docs/harness-v2-format.md`
- Modify: `README.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Mark the final ownership model.**

Update the Harness V2 roadmap so Milestone 9/12 no longer claim completion while duplicate supervisor/journal paths remain. Document the actual canonical session/lane API, supervisor boundary, event commit ordering, child-lane identity, and ACP exclusion.

- [ ] **Step 2: Document the operator-visible recovery behavior.**

Keep the existing format details and add the response for storage faults, malformed complete records, torn tails, unsafe tool interruption, durable abort, and explicit suspended-operation resume/abort.

- [ ] **Step 3: Update repository guidance.**

Add a durable rule that production code must use the canonical harness/session adapter for entries, records, queues, usage, and aborts; `HarnessSupervisor` may only schedule explicit tasks and project harness state.

- [ ] **Step 4: Commit documentation.**

```bash
git add harness_v2.md docs/harness-v2-format.md README.md AGENTS.md
git commit -m "docs: document canonical agent ownership"
```

### Phase 5 gate

Run:

```bash
cargo test --workspace
cargo check -p threadlane
git diff --check
```

For any Makepad-facing event or session changes, run the supported Studio flow and visually inspect normal run, tool run, stop, task status, subagent activity, and session switching. Do not claim visual verification unless the application was observed.

## Final audit

- [ ] Search production Rust for direct `SessionTree` appends, `append_op_record_to_file`, `load_op_records_from_file`, raw supervisor sequence allocation, and duplicate journal adapters; each result is either deleted or documented as compatibility-only.
- [ ] Confirm foreground, explicit `/task`, and model subagent paths all construct one canonical session harness.
- [ ] Confirm ACP remains on its existing transport runtime.
- [ ] Run the full gates from the approved design and inspect the final diff for generated files or unrelated edits.
- [ ] Verify every plan item is complete before reporting delivery.
