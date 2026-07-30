# Harness Observability UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire harness lifecycle observability into Threadlane’s existing chat activity and session-sidebar surfaces without adding a dashboard or new persistence layer.

**Architecture:** Extend the existing `AgentEvent` stream with concise recovery lifecycle events, reduce live and persisted lane information into one session-scoped presentation model, and render it through the existing `SubagentRail`/activity rows. Add one muted sidebar health badge for unresolved recovery or unsafe aborts; keep raw operation records and internal IDs hidden.

**Tech Stack:** Rust, Tokio event streams, Makepad, existing `ChatData`, `DisplayRow`, `SubagentRail`, `SessionListRow`, theme tokens, and current `GuiAgentEvent` routing.

## Global Constraints

- Use the existing `GuiAgentEvent::GenerationAgent` path; add no second UI event bus.
- Use `SessionWorkspace`/`ChatData` as the session-scoped presentation cache; add no global harness registry.
- Reuse the existing grouped `Working`/`Worked` activity presentation and `SubagentRail`.
- Keep raw operation IDs, sequence numbers, JSON records, and replay classifications out of normal UI copy.
- Healthy sessions receive no new sidebar decoration.
- Recovery and unsafe-abort states must be visible with concise accessible text, not color alone.
- Live and persisted lane events with the same durable lane key must update one presentation item rather than duplicate it.
- Preserve existing chat history, session-tree semantics, focus behavior, and background-task UI.
- Use existing theme role tokens and add no dependency or persistence backend.
- Run runtime visual verification for activity expansion, badge placement, and keyboard focus; compilation is insufficient for these UI changes.

---

### Task 1: Define the session-scoped harness presentation model

**Files:**
- Modify: `crates/threadlane-agent/src/events.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Modify: `crates/threadlane/src/panels/chat/state.rs`
- Modify: `crates/threadlane/src/workspace/mod.rs`
- Test: `crates/threadlane/src/panels/chat/state.rs`

**Interfaces:**

Add the minimal event payload needed by the existing stream:

```rust
pub enum AgentEvent {
    // existing variants...
    SubagentRecovery {
        run_id: String,
        status: SubagentRecoveryStatus,
        detail: Option<String>,
    },
}

pub enum SubagentRecoveryStatus {
    Started,
    Recovered,
    Retrying,
    Aborted,
}
```

Add a presentation-only model near the existing `SubagentRailItem` types:

```rust
pub enum HarnessActivityStatus {
    Queued,
    Working,
    Recovering,
    Recovered,
    Retrying,
    Aborted,
    Cancelled,
}

pub struct HarnessActivity {
    pub key: String,
    pub task: String,
    pub agent: String,
    pub status: HarnessActivityStatus,
    pub detail: String,
}

pub fn reduce_harness_activity(
    activities: &mut Vec<HarnessActivity>,
    activity: HarnessActivity,
)
```

- [ ] Write failing reducer tests for queued → working, working → recovered, retrying, aborted, terminal replacement, and duplicate-key replacement.
- [ ] Run `rtk cargo test -p threadlane --lib harness_activity`; verify the tests fail because the model/reducer does not exist.
- [ ] Add the event/status types and reducer. Use the durable lane key as the map identity, preserve insertion order for distinct lanes, and replace only the matching item.
- [ ] Add `harness_activities: Vec<HarnessActivity>` to the existing session-scoped `ChatData` or `SessionWorkspace` state, with an empty default and no persistence record.
- [ ] Emit `SubagentRecovery::Started`, `Recovered`, `Retrying`, and `Aborted` from the existing CodingAgent recovery path. Emit only concise detail strings suitable for UI copy.
- [ ] Run `rtk cargo test -p threadlane --lib harness_activity` and the focused coding-agent recovery tests; expect the reducer and event construction tests to pass.
- [ ] Commit with `feat: add harness activity presentation state`.

---

### Task 2: Route lifecycle events through the existing app event path

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/panels/chat/state.rs`
- Test: `crates/threadlane/src/app/mod.rs`
- Test: `crates/threadlane/src/panels/chat/state.rs`

**Interfaces:**

Extend the existing `App::handle_agent_event` match without creating another event channel:

```rust
AgentEvent::SubagentQueued { .. } => { /* reduce Queued */ }
AgentEvent::SubagentStarted { .. } => { /* reduce Working */ }
AgentEvent::SubagentFinished { .. } => { /* reduce terminal status */ }
AgentEvent::SubagentRecovery { .. } => { /* reduce recovery status */ }
```

- [ ] Write a failing app-state test that sends queued, started, recovery, and finished events for one session and asserts one activity item remains with the latest terminal status.
- [ ] Run `rtk cargo test -p threadlane --lib harness_event_routing`; verify failure because the app currently ignores subagent lifecycle variants.
- [ ] Route all subagent lifecycle variants in `handle_agent_event` to the target `SessionWorkspace` selected by the existing generation key.
- [ ] Keep stale-generation filtering unchanged; events for non-current generations must not mutate the active workspace.
- [ ] Map `SubagentFinished` errors to `Aborted` only when the event indicates cancellation/unsafe interruption; ordinary provider failure should remain a concise retryable/failed detail.
- [ ] Redraw the existing chat list after state reduction and preserve current session status/composer behavior.
- [ ] Run `rtk cargo test -p threadlane --lib harness_event_routing` and the existing app/state tests.
- [ ] Commit with `feat: route harness lifecycle events to chat state`.

---

### Task 3: Render harness activity through the existing chat UI

**Files:**
- Modify: `crates/threadlane/src/panels/chat/state.rs`
- Modify: `crates/threadlane/src/panels/chat/view.rs`
- Modify: `crates/threadlane/src/components/tool_fold_header.rs` only if the existing header needs a status label hook
- Test: `crates/threadlane/src/panels/chat/state.rs`
- Test: `crates/threadlane/src/panels/chat/view.rs`

**Interfaces:**

Extend the existing `SubagentRailItem`/activity summary path rather than adding a panel:

```rust
pub fn harness_activity_label(activity: &HarnessActivity) -> String;
pub fn harness_activity_detail(activity: &HarnessActivity) -> String;
pub fn merge_harness_activities(
    rail_items: &mut Vec<SubagentRailItem>,
    activities: &[HarnessActivity],
)
```

- [ ] Write failing presentation tests for `Delegated`, `Working`, `Recovering`, `Recovered`, `Retrying recovery`, `Aborted · unsafe tool`, and `Cancelled` copy.
- [ ] Add a test proving a persisted lane and a live event with the same key produce one rail item and retain the latest status/detail.
- [ ] Run `rtk cargo test -p threadlane --lib harness_activity_label`; verify failure before implementing the mapping.
- [ ] Extend `SubagentRailItem` with only the fields needed for durable identity and bounded status/detail; keep existing task/tool detail formatting intact.
- [ ] Merge session-scoped harness activities into the existing rail/display-row construction in `ChatList`/`DisplayRow` without adding a second collapsible container.
- [ ] Keep successful recovery collapsed by default, keep unresolved recovery visible, and bound task/detail text using existing normalization helpers.
- [ ] Ensure terminal status replaces the summary without deleting underlying `ChatMessage` history or changing stream grouping.
- [ ] Preserve keyboard focus, `ToolFoldHeader` layout-change redraws, and existing `SubagentRail::draw_all_unscoped` behavior.
- [ ] Run focused chat tests plus `rtk cargo test -p threadlane --lib panels::chat`.
- [ ] Commit with `feat: show harness activity in chat`.

---

### Task 4: Add the restrained session-sidebar health badge

**Files:**
- Modify: `crates/threadlane/src/panels/sessions/state.rs`
- Modify: `crates/threadlane/src/panels/sessions/view.rs`
- Modify: `crates/threadlane/src/components/mod.rs` only if a reusable badge registration is required
- Modify: `crates/threadlane/src/theme/mod.rs` only if an existing semantic role cannot represent the badge
- Test: `crates/threadlane/src/panels/sessions/state.rs`
- Test: `crates/threadlane/src/panels/sessions/view.rs`

**Interfaces:**

Use a small session-health projection derived from canonical session/workspace state:

```rust
pub enum SessionHealth {
    Healthy,
    Recovering,
    Warning,
}

pub fn session_health(activities: &[HarnessActivity]) -> SessionHealth;
```

- [ ] Write failing tests for healthy sessions, unresolved retryable recovery, and unsafe-abort warning visibility.
- [ ] Run `rtk cargo test -p threadlane --lib session_health`; verify failure before adding the projection.
- [ ] Add the projection in the existing sessions state path without adding a new global registry or persistence field.
- [ ] Render one muted badge in the existing session row template only for `Recovering` or `Warning`; do not alter healthy row geometry or selected-session treatment.
- [ ] Use existing semantic `warning`/`destructive`/`muted` theme roles and a short accessible label such as `Recovery pending` or `Subagent aborted`.
- [ ] Keep badge placement inside the existing bounded action/label slot so it cannot intercept row selection or context-menu events.
- [ ] Run focused session tests and `rtk cargo test -p threadlane --lib panels::sessions`.
- [ ] Commit with `feat: add session harness health badge`.

---

### Task 5: Validate runtime UX and finish the UI slice

**Files:**
- Modify: `AGENTS.md` only if implementation reveals a durable Makepad/UI convention
- Test: existing Threadlane chat/session test modules

- [ ] Run `rtk cargo test -p threadlane`.
- [ ] Run `rtk cargo check -p threadlane`.
- [ ] Run `rtk git diff --check`.
- [ ] Launch the desktop app and visually verify: normal chat remains dominant; activity rows expand/collapse without overlap; successful recovery is quiet; retryable recovery and unsafe abort remain visible; sidebar badge placement does not steal focus or break row/context-menu interaction.
- [ ] Verify stale-generation events do not update the active session and live/persisted lane updates do not duplicate rows.
- [ ] If runtime verification exposes a layout or event-routing defect, add a focused regression test before adjusting the UI.
- [ ] Commit any narrowly scoped validation/doc correction with `fix: polish harness observability UI`.

## Completion criteria

- Normal sessions render exactly as before unless harness activity exists.
- Existing chat activity grouping presents lifecycle summaries without a new panel.
- One durable lane produces one activity item across live and persisted updates.
- Recovery/unsafe-abort status is visible in chat and unresolved status is visible in the sidebar.
- Raw operation-log internals remain hidden.
- All targeted tests, `cargo test -p threadlane`, `cargo check -p threadlane`, and `git diff --check` pass.
- Runtime visual verification confirms expansion, badge placement, and focus behavior.
