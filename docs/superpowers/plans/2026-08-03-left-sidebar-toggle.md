# Left Sidebar Toggle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Let the left project/session sidebar collapse completely while leaving a compact expand button in the workspace.

**Architecture:** Reuse the existing `DockSplitter` as the single layout authority. `App` stores whether the left pane is open, changes the splitter alignment between `FromA(250.0)` and `FromA(0.0)`, and toggles a workspace-overlay `IconButton` for reopening.

**Tech Stack:** Rust, Makepad `DockFlat`, `DockSplitter`, existing theme and icon-button components.

## Global Constraints

- Keep the change focused in the existing app shell; do not add a new sidebar abstraction.
- Use role-based theme tokens and existing compact icon-button interaction states.
- Preserve the existing left pane width when open at the current 250px layout.
- Run `cargo check -p threadlane` and `git diff --check`; visual runtime verification is still required for placement.

---

### Task 1: Add the left-sidebar toggle state and controls

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs` around the `window_body`, dock definition, `App`, and `handle_actions`.

**Implementation:**

- Add `left_sidebar_open: bool` to `App`, defaulting to `true` during startup.
- Add `left_sidebar_toggle_btn` to the existing `sidebar_brand` row using `mod.components.IconButton`, with a compact sidebar icon and tooltip-free icon-only geometry.
- Add `left_sidebar_expand_btn` as a hidden overlay child of `window_body`, positioned at the top-left of the workspace and using the same icon/button styling.
- Handle both buttons in `handle_actions` through one `toggle_left_sidebar` method.
- In that method, call the existing dock reference’s `set_splitter_align` for `ids!(root)` with `SplitterAlign::FromA(250.0)` or `SplitterAlign::FromA(0.0)`, then update button visibility and redraw.
- Keep the reopen button above the dock so it remains interactive after the left pane reaches zero width.

### Task 2: Verify the implementation

**Files:**
- No additional test file unless compilation exposes a reusable pure helper worth testing.

**Checks:**

- Run `rtk cargo check -p threadlane`.
- Run `rtk git diff --check`.
- Inspect the diff to confirm no generated files or unrelated formatting changed.
- If the app can be launched in the Makepad runtime, verify open → collapsed → expanded placement and pointer interaction visually.
