# Git Branch Status Synchronization Design

## Problem

The workspace bottombar renders its branch label from `WorkspaceView.git_status`. Git inspection currently runs only when Git Review is opened, and asynchronous results are not associated with the project that requested them. After switching projects, the bottombar can therefore show the previous project's branch, and a delayed inspection can overwrite the current project's status.

## Scope

Synchronize the cached Git status with the active project whenever `active_work_dir` changes. Preserve the existing Git inspection implementation and explicit Git Review refresh behavior. Do not add polling or a second Git state store.

## Design

- Treat the active canonical work directory as the identity of a Git status request.
- On an active project change, clear the current cached `git_status` immediately so stale branch, change counts, and PR information are not displayed.
- Start an asynchronous `threadlane_git::inspect` for the newly active work directory.
- Carry the requested work directory alongside the asynchronous result.
- Apply a completed result only when its requested directory still equals the current active directory. Ignore stale results from prior projects.
- Reuse the same guarded refresh path when Git Review is opened.
- Notify the workspace after invalidation and after an accepted result so the bottombar redraws.

## Data flow

1. `AppState` changes `active_work_dir` during project/session selection.
2. `WorkspaceView` observes the model and detects the directory transition.
3. `WorkspaceView` clears `git_status`, requests inspection for the new directory, and notifies the view.
4. The background inspection sends `(requested_work_dir, result)` through the existing channel.
5. The workspace event loop accepts the result only if the requested directory remains active.
6. The status bar renders the accepted status; otherwise it renders the neutral fallback while inspection is pending.

## Error handling

Inspection errors remain non-fatal and clear the status for the requested project. An error or success result for an inactive project is ignored. No credentials or additional persisted state are involved.

## Testing

Add focused tests around the status-result acceptance logic, including:

- switching projects invalidates the old cached status;
- a result from an inactive/previous project is ignored;
- a result for the active project is accepted;
- the branch and change counts are available after the active result is applied.

Run `cargo check -p threadlane-gpui`, relevant focused tests, and `git diff --check`.
