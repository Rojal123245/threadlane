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

### Fix Round 1

Fix note:

- Root cause: command completion accepted every selected command label the same way, so pressing Enter on exact `/model` only filled the composer and closed the popup.
- Fix: `Submit` on command completion now opens model completion when the selected label is `/model`; `Tab` still performs normal command autocomplete, and other command labels keep the existing behavior.
- Added regression test: `runtime::tests::enter_on_model_command_completion_opens_model_picker`.

Red output:

```text
$ cargo test -p threadlane-cli enter_on_model_command_completion_opens_model_picker -- --nocapture
running 1 test

thread 'runtime::tests::enter_on_model_command_completion_opens_model_picker' (16296769) panicked at crates/threadlane-cli/src/runtime.rs:412:9:
assertion failed: state.completion.visible
test runtime::tests::enter_on_model_command_completion_opens_model_picker ... FAILED

failures:

failures:
    runtime::tests::enter_on_model_command_completion_opens_model_picker

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s
```

Focused green output:

```text
$ cargo test -p threadlane-cli enter_on_model_command_completion_opens_model_picker -- --nocapture
running 1 test
test runtime::tests::enter_on_model_command_completion_opens_model_picker ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s
```

Full verification output:

```text
$ cargo test -p threadlane-cli
running 28 tests
test input::tests::maps_cli_keys_to_input_events ... ok
test input::tests::maps_resize_and_ignores_unhandled_events ... ok
test commands::tests::parses_model_and_reasoning_commands ... ok
test commands::tests::command_usages_are_the_shared_source_for_help_and_completion ... ok
test commands::tests::filters_command_labels_from_known_commands ... ok
test commands::tests::rejects_unknown_commands_and_extra_model_arguments ... ok
test commands::tests::filters_model_labels_case_insensitively ... ok
test runtime::tests::completion_keys_insert_navigate_and_cancel_commands ... ok
test runtime::tests::enter_on_model_command_completion_opens_model_picker ... ok
test runtime::tests::model_completion_filters_and_accepts_without_submitting ... ok
test runtime::tests::normal_prompt_and_running_behavior_stay_unchanged ... ok
test tests::enter_submits_only_when_idle_and_composer_is_nonempty ... ok
test tests::escape_cancels_generation_before_quitting ... ok
test tui::tests::terminal_cleanup_is_idempotent ... ok
test ui::render::tests::follow_tail_tracks_manual_scroll_back_to_end ... ok
test ui::state::tests::agent_lifecycle_updates_run_status ... ok
test ui::state::tests::cancellation_commits_partial_streaming_assistant_text ... ok
test ui::state::tests::closing_completion_clears_candidates_and_mode ... ok
test ui::state::tests::completion_selection_wraps_in_both_directions ... ok
test ui::state::tests::message_updates_append_to_one_streaming_assistant ... ok
test ui::state::tests::reducer_updates_errors_plan_subagents_and_cancellation ... ok
test ui::state::tests::test_message_types ... ok
test ui::state::tests::test_app_state_initialization ... ok
test ui::state::tests::tool_lifecycle_replaces_activity_status ... ok
test ui::render::tests::empty_activity_and_plan_do_not_create_empty_sections ... ok
test ui::render::tests::active_plan_and_activity_get_bounded_height ... ok
test commands::tests::rejects_mutating_commands_while_running ... ok
test commands::tests::model_command_persists_the_provider_prefixed_model ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.58s
```
