# Zero-Copy Chat Row Rendering Design

## Goal

Reduce allocation and CPU work while rendering chat transcript rows without changing transcript ordering, activity grouping, interaction behavior, or persisted session data.

## Scope

This change targets two hot paths in `threadlane-gpui`:

1. Rendering transcript messages and grouped tool activities without cloning complete `ChatMessageInfo` or `ToolActivityInfo` values.
2. Producing expensive display summaries and formatted JSON in state projection code rather than GPUI render methods.

The change does not redesign streaming state, Markdown rendering, transcript pagination, or persistence formats.

## Architecture

`AppState` remains the owner of canonical GPUI-facing projections. `ChatMessageInfo`, `ToolActivityInfo`, and `TrajectoryEntry` continue to contain display-ready values. No parallel render-view model or cache synchronization layer will be introduced.

`ChatListView` keeps an `Arc<Vec<ChatMessageInfo>>`. A transcript row clones this outer `Arc` into a local snapshot before borrowing a message or message range. This separates the data borrow from `self`, allowing render helpers that mutate view-local state to consume references without cloning complete messages.

Grouped activity rows receive a message slice and traverse nested `tool_activities` by reference. The traversal filters `update_plan` activities and preserves current order. The group uses allocation-free iterator passes to calculate its first identifier, count, visible offset, and rendered rows rather than collecting an owned temporary vector.

## Formatting Ownership

Tool activity summaries and details are generated when persisted or live agent events are projected into `ToolActivityInfo`. Render methods only select and display the precomputed values.

Trajectory entries likewise receive complete `summary` and `detail` strings in state projection paths. Pretty JSON serialization and diagnostic summary construction must not occur in `render_trajectory` or its row-building closures. State projections may remain lazy at the feature level, but expensive formatting belongs to projection helpers and is cached by the existing trajectory revision mechanism.

## Necessary Ownership

Small ownership conversions required by GPUI remain in place. Element IDs, callback captures, clipboard text, and owned child text may clone strings because their values must outlive the render stack. The optimization specifically removes deep clones of complete message and activity records and temporary owned activity collections.

## Behavior and Error Handling

The rendered output must remain unchanged:

- Message order and row identity remain stable.
- Consecutive activity-only messages remain one group.
- `update_plan` activities remain hidden from activity groups.
- Collapsed groups show the same recent-activity limit.
- Expanding activities and copying messages continue to work.
- Invalid tool argument JSON falls back to the existing display behavior during projection.
- Failed pretty serialization uses the existing debug fallback rather than failing rendering.

## Testing

Focused unit tests will cover allocation-free activity traversal behavior through observable results: filtering, ordering, group counts, and recent-item selection. Existing transcript row tests will continue to validate grouping. The implementation will be verified with:

```bash
cargo test -p threadlane-gpui
cargo check -p threadlane-gpui
git diff --check
```

## Non-Goals

- Converting all display strings to `Arc<str>` or `SharedString`.
- Removing GPUI-required callback and element ownership clones.
- Changing the transcript data model or persisted JSONL schema.
- Adding a second presentation cache.
- Optimizing Markdown parsing or stream batching in this change.
