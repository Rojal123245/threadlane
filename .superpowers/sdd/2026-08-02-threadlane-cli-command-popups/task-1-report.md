Task 1 report — 2026-08-02

Scope completed:
- Updated `crates/threadlane-cli/src/state.rs`
- Updated `crates/threadlane-cli/src/commands.rs`

What changed:
- Added a minimal `CompletionState` plus `CompletionMode` to `AppState`.
- Added small state helpers to show, close, and wrap completion selection.
- Added shared command metadata in `commands.rs` and reused it for help text.
- Added pure helpers for command-label and model-label filtering.
- Added focused unit tests for:
  - command filtering
  - model filtering
  - completion selection wrapping
  - closing completion

Verification:
- `cargo test -p threadlane-cli` → 23 passed
- `git diff --check` → clean

Notes:
- Kept the change limited to `state.rs` and `commands.rs`.
- Reused the existing command set; no new command execution path was added.

Fix round 1 — 2026-08-02

Reviewer issue addressed:
- Removed the duplicate `COMMAND_DESCRIPTIONS`/`CommandDescription` catalog.
- Removed the unused speculative `description` field.
- Kept only the minimal shared APIs Task 1 needs:
  - `command_usages()`
  - `filter_command_labels(...)`
  - `filter_model_labels(...)`

Implementation:
- `commands.rs` now uses shared command name/label/usage constants.
- `parse_command(...)` reuses those command-name constants.
- help output reuses `command_usages()`.
- command completion labels reuse the shared label constants.

Verification command/output:
- Command: `cargo test -p threadlane-cli`
- Output: `cargo test: 24 passed (1 suite, 0.90s)`
- Command: `git diff --check`
- Output: clean
