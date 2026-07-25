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
