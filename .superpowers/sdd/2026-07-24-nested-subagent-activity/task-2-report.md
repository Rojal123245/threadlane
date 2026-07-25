# Task 2: Expandable subagent activity rows

## Delivered

- Child tool calls tagged by Task 1 are omitted from the parent activity group and are passed with the full chat message list to `subagent_rail_items`.
- Each subagent task rail row now uses `ToolFoldHeader`, with its agent, task preview, textual/visual status, and activity markdown bound independently.
- `ChatList` recognizes `ToolFoldHeaderAction::LayoutChanged` from nested rows and redraws the parent portal list for reflow.

## Validation

- `cargo test -p threadlane panels::chat::state::tests::subagent_rail_places_tagged_child_tool_under_its_task` — passed: 1 test, 82 filtered out.
- `cargo check -p threadlane` — passed: 0 errors, 2 existing Makepad duplicate-package warnings. The task brief's anticipated sidebar errors did not reproduce in the current shared tree.
- `git diff --check` — passed.

## Remaining caveat

The Makepad compile check validates the script syntax, but the nested expansion was not visually exercised in a running desktop app.

## Fix round 1

- Completed rail items now build their expandable detail from persisted session thinking, inner tool activity, and final report instead of requiring an active child-tool run ID.
- `cargo test -p threadlane panels::chat::state::tests::completed_subagent_rail_items_keep_their_persisted_detail` — passed: 1 test, 83 filtered out.
- `cargo test -p threadlane panels::chat::state::tests::subagent_result_markdown_keeps_each_agent_report_and_tool_outcome` — passed: 1 test, 83 filtered out.
- `cargo check -p threadlane` — passed: 0 errors, 2 Makepad duplicate-package warnings.
- `git diff --check` — passed.

## Final review fix

- Child tool tags are now suppressed only when the transcript also renders an outer subagent tool; an orphaned or stale tagged tool remains ordinary top-level activity.
- `cargo test -p threadlane panels::chat::view::tests::child_tool_rows_are_hidden_only_when_a_subagent_parent_is_rendered` — passed: 1 test, 84 filtered out.
- `cargo test -p threadlane panels::chat::state::tests::subagent_rail_places_tagged_child_tool_under_its_task` — passed: 1 test, 84 filtered out.
- `cargo check -p threadlane` — passed: 0 errors, 2 Makepad duplicate-package warnings.
- `git diff --check` — passed.
