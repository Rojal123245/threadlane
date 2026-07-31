# Composer Checkout Target Design

## Goal

Expose the agent’s Git target directly beneath the composer without increasing composer clutter.

## Design

Move the existing branch picker from the composer footer into a compact row directly beneath the composer. Add a checkout-target selector with `Current checkout` as the default and `New worktree…` as the alternative. Selecting the worktree option opens a small inline name/path prompt with Cancel and Create actions. Creation occurs only after confirmation; the resulting worktree becomes the selected target and Git state is refreshed.

The selected target is session-scoped. Existing current-checkout behavior remains unchanged, and cancelling the prompt leaves the current target untouched. Worktree creation uses the repository’s existing Git command helpers and does not introduce a new persistence format.

## Scope

- Add the compact target row and prompt UI using existing Makepad components.
- Move and reuse the existing branch picker.
- Add Git worktree creation and target state wiring.
- Keep the composer footer controls unchanged except for removing the relocated branch picker.

## Verification

- Unit-test target selection and cancel behavior.
- Unit-test worktree name/path validation and command construction.
- Run `cargo test -p threadlane --bin threadlane`, `cargo check -p threadlane`, and `git diff --check`.
- Perform a fresh runtime UI check when the development binary is available.

