# Subagent Activity Design

## Goal

Show each delegated agent's live tool activity inside its own expandable row, rather than as detached entries in the parent chat activity group.

## Design

The coding-agent relay will preserve a stable child-agent identifier on child tool events. The chat state will associate those events with the outer `subagent` tool call and its matching agent task. The `SubagentMsg` row will render one collapsible child row per task; its body contains that agent's tool activity while running and its persisted reasoning/report after completion.

The parent chat's normal activity grouping will exclude relayed child events. Parent tools and non-subagent activity retain their current rendering and persistence behavior.

## Interaction

Each task row is a `ToolFoldHeader`: click its header to toggle only that task's details. Its layout-change action redraws the containing `PortalList`, preserving the existing chat-row reflow behavior.

## Error Handling

An unknown or malformed child-agent identifier remains a normal parent activity entry instead of being dropped. Failed child tools retain their error status in the matching agent row.

## Testing

Add a state-level regression test that verifies child events are associated with their owning task and excluded from parent activity grouping. Retain the focused coding-agent relay test coverage for child tool-event identity.
