# Threadlane UI Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove dead and duplicated Makepad UI code, simplify view-state synchronization, and normalize touched interaction styling without changing product behavior.

**Architecture:** Work in small independent phases. Delete proven-dead modules first, then simplify chat and settings paths using existing components, then normalize theme/state usage. Visual geometry changes are gated on fresh Makepad Studio evidence.

**Tech Stack:** Rust, Makepad `script_mod!`, existing Threadlane theme and components.

## Global Constraints

- Preserve existing uncommitted harness and activity-rail work.
- Add no dependency, framework, persistence path, or provider behavior.
- Reuse existing components before creating or extending one.
- Keep model-dropdown anchor geometry unchanged.
- Every task ends with focused tests, `cargo check -p threadlane`, and `git diff --check`.

---

### Task 1: Delete proven-dead UI scaffolding

**Files:**
- Delete: `crates/threadlane/src/panels/tasks/state.rs`
- Delete: `crates/threadlane/src/panels/tasks/view.rs`
- Delete: `crates/threadlane/src/panels/tasks/mod.rs`
- Delete: `crates/threadlane/src/panels/settings/view.rs`
- Delete: `crates/threadlane/src/panels/settings/mod.rs`
- Delete: `crates/threadlane/src/panels/files/view.rs`
- Delete: `crates/threadlane/src/panels/files/mod.rs`
- Delete: `crates/threadlane/src/panels/git/state.rs`
- Modify: `crates/threadlane/src/panels/git/mod.rs`
- Modify: `crates/threadlane/src/panels/mod.rs`
- Modify: `crates/threadlane/src/components/mod.rs`
- Delete unused component files only after a zero-call-site search.

**Interfaces:**
- Preserve `panels::git::view` and all active component templates.
- Remove only symbols whose repository-wide caller count is zero.

- [ ] Search every candidate symbol and template by exact name.
- [ ] Delete the dead files, module declarations, and script registrations.
- [ ] Run `cargo check -p threadlane` and `git diff --check`.

### Task 2: Consolidate chat view synchronization

**Files:**
- Modify: `crates/threadlane/src/panels/chat/view.rs`
- Modify: `crates/threadlane/src/app/mod.rs` only for reusable fold-row inheritance proven valid by a Studio script check.

**Interfaces:**
- Preserve `DisplayRow`, transcript ordering, `PortalList` range, auto-tail, `ThinkingMsg`, `ToolMsg`, and `SubagentRail` behavior.
- Add one private helper that synchronizes jump-layer, typed button, and hint visibility/redraw state.

- [ ] Add a focused unit test for the pure jump visibility decision if logic changes.
- [ ] Replace repeated jump widget synchronization with the helper.
- [ ] Reuse `ChatFoldRowBase` only if it reduces DSL and compiles; otherwise delete the unused base.
- [ ] Run all `panels::chat::view::tests`, `cargo check -p threadlane`, and `git diff --check`.

### Task 3: Simplify settings page state

**Files:**
- Modify: `crates/threadlane/src/components/provider_settings_modal.rs`
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/components/capability_row.rs` only if empty-row reuse needs a shared label ID.

**Interfaces:**
- Preserve `SettingsPage`, `set_page`, `open_page`, all navigation IDs, and the shared PortalList draw loop.
- Use one table for nav selected state and one `match` for visible page widgets.

- [ ] Add tests for the selected-page mapping as a pure helper.
- [ ] Replace repeated page booleans and navigation visibility calls.
- [ ] Reuse the existing empty-row template where IDs and copy allow it; do not introduce a new empty-state component.
- [ ] Run provider-settings tests, `cargo check -p threadlane`, and `git diff --check`.

### Task 4: Normalize touched visual states and theme usage

**Files:**
- Modify: `crates/threadlane/src/components/tool_fold_header.rs`
- Modify: `crates/threadlane/src/components/command_input.rs`
- Modify: `crates/threadlane/src/panels/chat/view.rs`
- Modify: `crates/threadlane/src/theme/mod.rs` only if no existing semantic role fits.

**Interfaces:**
- Preserve current semantic colors and animations.
- Ensure touched buttons define hover, focus, pressed, and border states together.

- [ ] Replace hard-coded colors with existing role-based theme values where script access permits.
- [ ] Keep Rust-side runtime colors centralized rather than scattering new literals.
- [ ] Run focused component tests, `cargo check -p threadlane`, and `git diff --check`.

### Task 5: Fresh Studio visual audit

**Files:**
- Modify only views with an observed layout, focus, clipping, or density defect.

**Interfaces:**
- Preserve existing visual language, popup clamping, and interaction bounds.

- [ ] Start or connect to Makepad Studio on localhost and clear stale builds.
- [ ] Run Threadlane fresh; inspect transcript, settings, sidebars, dialogs, and narrow widths.
- [ ] Apply only evidence-backed spacing/focus corrections.
- [ ] Re-run Studio after each UI edit.
- [ ] Run `cargo test --workspace -- --test-threads=1`, `cargo check -p threadlane`, and `git diff --check`.
