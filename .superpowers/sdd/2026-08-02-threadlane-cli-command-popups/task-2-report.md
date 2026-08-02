### Task 2 Report

Implemented popup-aware CLI input dispatch for command and model completions.

- Added/kept key mapping for `Tab`, `Previous`, and `Next` input events without exposing Crossterm outside `input.rs`.
- Routed `/` command completion and `/model` model completion through existing `filter_command_labels` / `filter_model_labels` and `CompletionState` APIs.
- Added completion-aware `Tab`, `Up`, `Down`, `Enter`, and `Escape` behavior.
- Made `/model` open model completion instead of submitting immediately.
- Loaded `available_models()` lazily when model completion is first needed; empty catalogs fall back to the current model in the TUI loop, and direct dispatch reports that no models were found.
- Preserved normal prompt submission, transcript scrolling via Up/Down when no popup is open, and running-state cancel/ignore behavior.
- Did not touch `render.rs`.

Verification:

- Red check: `cargo test -p threadlane-cli dispatch_input -- --nocapture` initially failed to compile on stale `ScrollUp` / `ScrollDown` runtime matches.
- Final check: `cargo test -p threadlane-cli` passed: 27 passed, 0 failed.
- Whitespace check: `git diff --check` passed.
