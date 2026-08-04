# Chat Jump-to-Latest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a compact left-edge jump-to-latest control to the chat list.

**Architecture:** Keep the control in the existing `ChatList` overlay. Derive visibility from the PortalList’s current end state, route the click through a typed local widget action, and use the existing `IconButton` plus theme roles for styling.

**Tech Stack:** Rust, Makepad `script_mod!`, `PortalList`, existing Threadlane components.

## Global Constraints

- Reuse existing components and theme tokens.
- Keep the control outside transcript layout.
- Preserve manual scrolling; only show the control when older content exists below the viewport.
- Verify with `cargo check -p threadlane` and `git diff --check`.

### Task 1: Add the floating control

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/panels/chat/view.rs`

- [x] Add the left-edge overlay button and preview label using existing components.
- [x] Update visibility from `PortalList::is_at_end()` during draw and scroll actions.
- [x] Handle the typed click by enabling the tail range and scrolling to the latest row.
- [x] Run the focused compile and whitespace checks.
