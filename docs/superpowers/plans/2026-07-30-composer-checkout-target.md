# Composer Checkout Target Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users choose the current checkout or create a named/path-based worktree from a compact row beneath the composer.

**Architecture:** Reuse `GitBranchDropDown`, existing Git status/operation plumbing, and Makepad `SearchInput`/`HeaderChipButton` controls. Add a small session-scoped checkout target state in `App`; create worktrees through a validated `git worktree add` helper, then refresh the target and status UI after success.

**Tech Stack:** Rust, Makepad `script_mod!`, existing Git command wrapper, session/workspace state.

## Global Constraints

- Current checkout remains the default and existing branch checkout behavior remains intact.
- New worktrees are created only after explicit Create confirmation.
- Validate worktree names/paths and keep command execution inside the selected repository context.
- Keep the target row compact and directly below the composer.
- Do not add a new dependency or a second persistence format.

---

### Task 1: Add validated worktree creation

**Files:**
- Modify: `crates/threadlane/src/git.rs`
- Test: `crates/threadlane/src/git.rs` tests

**Interfaces:**
- Consumes: repository path, worktree path, and branch/ref name.
- Produces: `pub fn create_worktree(work_dir: &Path, path: &Path, branch: &str) -> Result<(), GitError>`.

- [ ] Add failing tests for empty/relative/escaping paths and command argument validation using the existing Git validation patterns.
- [ ] Implement the smallest wrapper around `git worktree add`, rejecting an empty branch and paths that are not absolute or do not remain within the intended project parent.
- [ ] Run `cargo test -p threadlane --bin threadlane git::tests` and confirm all Git tests pass.
- [ ] Commit with `feat: add validated git worktree creation`.

### Task 2: Add target prompt state and controls

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs` script UI and `App` state
- Modify: `crates/threadlane/src/workspace/mod.rs` only if target state must be session-scoped there
- Test: `crates/threadlane/src/app/mod.rs` tests

**Interfaces:**
- Consumes: active project path, current branch picker labels, and Git operation state.
- Produces: target selector, worktree name/path prompt, and confirmed target state.

- [ ] Add a `checkout_target_row` immediately below `input_bar`, containing a compact `Current checkout` selector and the existing `git_branch_drop`.
- [ ] Remove the branch picker from `composer_footer` without changing the remaining footer controls.
- [ ] Add an inline `worktree_prompt_row` with name/path inputs and Cancel/Create buttons; keep it hidden until `New worktree…` is selected.
- [ ] Add state for the prompt visibility and selected target path, preserving the current checkout as the default.
- [ ] Add pure tests for default target, cancelling the prompt, and selecting a confirmed worktree target.
- [ ] Run the focused app tests and verify the new controls compile.
- [ ] Commit with `feat: add composer checkout target controls`.

### Task 3: Wire creation, selection, and refresh

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/panels/git/view.rs` only if target labels need a shared formatter

**Interfaces:**
- Consumes: Task 1 `create_worktree`, target prompt state, and existing `start_git_operation`/`request_git_status` flows.
- Produces: confirmed worktree creation, selected target label, branch checkout behavior, and refreshed Git UI.

- [ ] Handle `New worktree…` selection by showing the prompt, clearing prior fields, and focusing the name input.
- [ ] On Create, validate name/path, derive the selected branch/ref, call `create_worktree` through the existing background Git operation path, and keep the target unchanged while the operation is pending.
- [ ] On success, set the selected target to the created path, close the prompt, refresh Git status for that path, and redraw the target row.
- [ ] On failure, keep the prompt open and show the existing Git feedback error without losing entered values.
- [ ] Handle Cancel and Escape/outside dismissal by closing the prompt without creating a worktree.
- [ ] Ensure branch selection still uses the existing checkout path when the target is `Current checkout`.
- [ ] Run focused tests for target handling and the full app test binary.
- [ ] Commit with `feat: wire composer worktree creation`.

### Task 4: Verify UI and regression safety

**Files:**
- No new source files.

- [ ] Run `cargo test -p threadlane --bin threadlane`.
- [ ] Run `cargo check -p threadlane`.
- [ ] Run `git diff --check`.
- [ ] Run a fresh development UI build and verify the row sits directly below the composer, the prompt is compact, Cancel is non-destructive, and Create updates the selected target.
- [ ] Confirm `git status --short` is clean and no generated or packaged runtime files changed.

