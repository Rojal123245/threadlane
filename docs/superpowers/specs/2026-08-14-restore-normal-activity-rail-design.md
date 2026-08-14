# Restore Normal Activity Rail

## Goal

Show normal thinking and tool activity as the existing individual collapsible
`ThinkingMsg` and `ToolMsg` rows while preserving the current subagent rail.

## Design

- Stop converting consecutive normal thinking and tool messages into
  `ActivityGroupMsg`.
- Map each normal thinking message to its existing `ThinkingMsg` template.
- Map each normal tool message to its existing `ToolMsg` template.
- Keep subagent tools and harness lifecycle entries routed through
  `SubagentMsg` and `SubagentRail`.
- Remove the now-unused normal activity-group construction code and template
  only where doing so does not affect another caller.

## Verification

- Add a focused display-row test proving consecutive thinking and tool messages
  remain separate rows.
- Retain existing subagent rail tests.
- Run the focused chat tests, `cargo check -p threadlane`, and
  `git diff --check`.
- Use a fresh Makepad Studio run to verify the two collapsible normal activity
  rows and the subagent rail render independently.
