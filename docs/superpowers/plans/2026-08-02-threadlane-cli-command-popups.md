# Threadlane CLI Command Popups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add keyboard-first command autocomplete and an interactive model picker to the existing Threadlane CLI TUI.

**Architecture:** Keep command metadata and filtering in `commands.rs`; keep popup selection state in `AppState`; map popup navigation through the existing `InputEvent` path; render one reusable popup above the composer. `/` opens command completion, `/model` opens model completion, and `/model <text>` filters models locally after the model catalog is loaded.

**Tech Stack:** Rust, Ratatui, Crossterm, existing `CodingAgent::available_models`, Tokio runtime.

## Global Constraints

- Do not add a new UI framework or command registry.
- Preserve the existing `input -> runtime -> state -> render` boundaries.
- Keep normal prompt submission unchanged.
- Mutating model/reasoning commands remain unavailable while generation is running.
- Verify with focused CLI tests, `cargo check -p threadlane-cli`, and `git diff --check`.

---

### Task 1: Add completion state and command/model candidates

**Files:**
- Modify: `crates/threadlane-cli/src/state.rs`
- Modify: `crates/threadlane-cli/src/commands.rs`
- Test: inline unit tests in both files

**Interfaces:**
- `commands.rs` produces command descriptions and filtered model/command labels.
- `AppState` owns popup visibility, candidate labels, selected index, and the current completion mode.

- [ ] **Step 1: Write failing tests** for command filtering, model filtering, selection wrapping, and closing completion.
- [ ] **Step 2: Run `cargo test -p threadlane-cli`** and confirm the new tests fail.
- [ ] **Step 3: Implement the smallest `CompletionState` and pure filtering helpers.** Use the existing command parser’s command set as the source of truth; do not duplicate command execution logic.
- [ ] **Step 4: Run `cargo test -p threadlane-cli`** and confirm the tests pass.
- [ ] **Step 5: Commit with `feat: add cli completion state`**.

### Task 2: Route keyboard input through completion mode

**Files:**
- Modify: `crates/threadlane-cli/src/input.rs`
- Modify: `crates/threadlane-cli/src/runtime.rs`
- Test: inline unit tests in `input.rs` and `runtime.rs`

**Interfaces:**
- Input mapping adds `Tab`, `Previous`, and `Next` events without exposing Crossterm details to runtime behavior.
- Runtime opens command completion when the composer is `/`, opens model completion for `/model`, filters on characters/backspace, and accepts/cancels with Enter/Escape.

- [ ] **Step 1: Write failing key-mapping and completion-navigation tests.** Cover Tab, Up/Down, Escape, command insertion, and model selection.
- [ ] **Step 2: Run the focused tests and confirm failure.**
- [ ] **Step 3: Implement popup-aware dispatch before normal submit/scroll behavior.** `/model` must not submit immediately; selecting an item must replace the composer with the selected command/model and close completion.
- [ ] **Step 4: Load the model catalog only when model completion opens, then filter it in memory.** Preserve the current model when the catalog is unavailable and show a useful message rather than panicking.
- [ ] **Step 5: Run focused CLI tests and confirm pass.**
- [ ] **Step 6: Commit with `feat: navigate cli completions`**.

### Task 3: Render command and model popups

**Files:**
- Modify: `crates/threadlane-cli/src/render.rs`
- Modify: `crates/threadlane-cli/src/state.rs` only if layout needs derived popup height
- Test: inline layout/render-state tests

**Interfaces:**
- `render.rs` consumes `AppState.completion` and draws a bounded popup immediately above the prompt.
- Popup rows show command name plus short description, or model name; selected row uses the existing yellow accent.

- [ ] **Step 1: Write failing layout tests** for popup visibility, bounded height, and no-popup layout.
- [ ] **Step 2: Implement popup layout and rendering** without changing transcript/activity/plan behavior.
- [ ] **Step 3: Keep the popup above the composer and clamp its height to the available terminal space.**
- [ ] **Step 4: Run `cargo test -p threadlane-cli` and inspect `cargo run -p threadlane-cli -- --help`.**
- [ ] **Step 5: Commit with `feat: render cli completion popups`**.

### Task 4: Final verification and cleanup

**Files:**
- Modify only files required by test failures or formatting.

- [ ] **Step 1: Run `cargo test -p threadlane-cli`**.
- [ ] **Step 2: Run `cargo check -p threadlane-cli`**.
- [ ] **Step 3: Run `git diff --check` and verify no unrelated files changed.**
- [ ] **Step 4: Manually verify `/`, `/model`, filtering, Tab, Up/Down, Enter, and Escape in a real terminal when available.**
- [ ] **Step 5: Commit the final integration with `feat: add cli command and model popups`**.
