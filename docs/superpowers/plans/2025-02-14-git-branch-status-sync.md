# Git Branch Status Synchronization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep the workspace bottombar Git branch and status synchronized automatically with the active project, without allowing stale asynchronous results to overwrite the current project.

**Architecture:** `WorkspaceView` remains the owner of transient Git status. It tracks the project path associated with each background inspection, invalidates the cached status when the model's active project changes, and accepts results only when their request path still equals the active path. Existing `threadlane_git::inspect` and the existing event loop remain in use.

**Tech Stack:** Rust, GPUI entities/observers, `threadlane_git`, existing `WorkspaceView` tests.

## Global Constraints

- Reuse the existing `threadlane_git::inspect` implementation and `GitStatus` type.
- Do not add polling, a second Git registry, persistence, or new dependencies.
- Keep asynchronous Git work off the UI thread.
- Never display a previous project's branch after the active project changes.
- Run `cargo check -p threadlane-gpui` and `git diff --check` before completion.

---

### Task 1: Add a testable guarded Git-status result path

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/workspace/view.rs:36-38, 306-315`
- Test: `crates/threadlane-gpui/src/screens/workspace/view.rs` test module or the nearest existing workspace tests

**Interfaces:**
- Consume the existing `GitStatus`, `GitEvent`, `AppState.active_work_dir`, and `WorkspaceView.git_status`.
- Produce a `GitEvent::Loaded` variant carrying the requested `PathBuf` and `Result<GitStatus, String>`, plus a small acceptance helper or equivalent method that can be unit-tested without starting a thread.

- [ ] **Step 1: Inspect the existing workspace test module and determine the smallest construction-free test seam.**

Read the bottom of `crates/threadlane-gpui/src/screens/workspace/view.rs` and any existing tests around workspace/project state. Reuse existing test helpers; do not instantiate a GPUI window solely to test path matching.

- [ ] **Step 2: Write the failing tests for active and stale results.**

Add tests expressing this exact behavior using the existing `GitStatus` construction helpers if available:

```rust
#[test]
fn git_result_is_accepted_only_for_active_work_dir() {
    let active = PathBuf::from("/projects/current");
    let stale = PathBuf::from("/projects/previous");

    assert!(git_result_matches_active(&active, &active));
    assert!(!git_result_matches_active(&stale, &active));
}
```

If a separate invalidation helper is introduced, test it using the repository's existing `GitStatus` fixture pattern:

```rust
#[test]
fn changing_work_dir_invalidates_cached_git_status() {
    let mut status = existing_git_status_fixture();
    invalidate_git_status(&mut status);
    assert!(status.is_none());
}
```

If no existing fixture exists, test invalidation through the existing `WorkspaceView`/state test setup rather than inventing a production-only fixture constructor.

- [ ] **Step 3: Run the focused test and verify it fails for the intended reason.**

Run:

```bash
cargo test -p threadlane-gpui git_result_is_accepted_only_for_active_work_dir -- --nocapture
```

Expected: compilation failure because the new test helper/guard does not yet exist.

- [ ] **Step 4: Implement the smallest pure path-matching/invalidation helpers or equivalent inline logic.**

Use `Path` equality against the canonical `active_work_dir`; do not use string comparison or lexical normalization. Keep helpers private and focused. The intended guard is equivalent to:

```rust
fn git_result_matches_active(requested: &Path, active: &Path) -> bool {
    requested == active
}
```

Change the event shape to carry the request path:

```rust
enum GitEvent {
    Loaded {
        work_dir: PathBuf,
        result: Result<GitStatus, String>,
    },
}
```

Update the producer and consumer enough for the focused tests to compile.

- [ ] **Step 5: Run the focused tests and verify they pass.**

Run:

```bash
cargo test -p threadlane-gpui git_result_is_accepted_only_for_active_work_dir -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit the guarded result seam.**

```bash
git add crates/threadlane-gpui/src/screens/workspace/view.rs
git commit -m "test: guard git status by active project"
```

### Task 2: Refresh Git status automatically on project changes

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/workspace/view.rs:115-166, 212-218, 294-315`
- Test: `crates/threadlane-gpui/src/screens/workspace/view.rs` existing focused tests

**Interfaces:**
- Consume model observation callbacks and `AppState.active_work_dir`.
- Produce automatic background inspection on every active-directory transition, immediate cache invalidation, and stale-result rejection.

- [ ] **Step 1: Add a tracked active work directory to `WorkspaceView`.**

Add a field such as:

```rust
last_git_work_dir: Option<PathBuf>,
```

Initialize it from the current `AppState.active_work_dir` when constructing the view, or initialize it to `None` and let the first observed reconciliation request the initial status. Ensure the field represents the path used for the latest requested/active inspection, not a second source of truth for the active project.

- [ ] **Step 2: Add a single reconciliation method for active-project Git status.**

Implement a method with behavior equivalent to:

```rust
fn sync_git_status_with_active_project(&mut self, cx: &App) {
    let active_work_dir = self.model.read(cx).active_work_dir.clone();
    if self.last_git_work_dir == active_work_dir {
        return;
    }

    self.last_git_work_dir = active_work_dir.clone();
    self.git_status = None;

    if let Some(work_dir) = active_work_dir {
        self.spawn_git_status_refresh(work_dir);
    }
}
```

The method must clear the old status before spawning the new inspection. Use the existing `git_event_tx` and background thread. `spawn_git_status_refresh` must send the requested `work_dir` with the result.

- [ ] **Step 3: Invoke reconciliation from the existing model observer/event loop.**

The current observer only calls `cx.notify()`. Update the workspace update cycle so it compares/reconciles after model actions and before rendering. Avoid starting a new inspection on every 80ms loop; the tracked path comparison must make this transition-only.

The automatic path must work for project attach, project selection, and session selection whenever `active_work_dir` changes.

- [ ] **Step 4: Guard `apply_git_event` against stale results.**

For `GitEvent::Loaded { work_dir, result }`, read the current active path and return without changing `git_status` unless it equals the event's `work_dir`. For an accepted success, store the status. For an accepted error, clear the status. Notify the workspace after a status transition.

- [ ] **Step 5: Route explicit Git Review refresh through the same request path.**

Keep `open_git_dialog` behavior, but make `refresh_git_status` update the request identity and use the same event format/guard. If the active path is unchanged, explicit refresh should still clear/reload the status rather than being suppressed by transition-only reconciliation.

- [ ] **Step 6: Run focused tests and compile checks.**

Run:

```bash
cargo test -p threadlane-gpui git_result_is_accepted_only_for_active_work_dir -- --nocapture
cargo check -p threadlane-gpui
```

Expected: tests pass and the GPUI crate compiles. Fix only errors caused by the event-shape and synchronization changes.

- [ ] **Step 7: Commit the automatic synchronization implementation.**

```bash
git add crates/threadlane-gpui/src/screens/workspace/view.rs
git commit -m "fix: sync bottombar git status with project"
```

### Task 3: Verify the complete behavior and patch hygiene

**Files:**
- Modify only if verification exposes a direct issue: `crates/threadlane-gpui/src/screens/workspace/view.rs`

- [ ] **Step 1: Review the diff for unrelated changes.**

Run:

```bash
git diff HEAD~2..HEAD -- crates/threadlane-gpui/src/screens/workspace/view.rs
git status --short
```

Confirm that the implementation does not alter unrelated workspace UI or Git behavior.

- [ ] **Step 2: Run the complete relevant test/check set.**

Run:

```bash
cargo test -p threadlane-gpui
cargo check -p threadlane-gpui
git diff --check
```

Expected: all package tests pass, check succeeds, and `git diff --check` reports no whitespace errors.

- [ ] **Step 3: Confirm the synchronization invariants in code.**

Verify that:

- `git_status` is cleared immediately after active project changes;
- every automatic request carries its requested `PathBuf`;
- stale results cannot mutate `git_status`;
- accepted results cause a redraw;
- explicit Git Review refresh still works for the same project.

- [ ] **Step 4: Commit any narrowly scoped verification fix.**

If a direct implementation issue is found, add a focused test first, fix it, rerun the checks, and commit with:

```bash
git add crates/threadlane-gpui/src/screens/workspace/view.rs
git commit -m "fix: complete git status sync verification"
```
