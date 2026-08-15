# GPUI Sidebar Collapse

## Goal

Add a real sidebar collapse control beside the macOS traffic lights so the New Task action occupies its own unobstructed row.

## Design

`WorkspaceView` owns a non-persisted `sidebar_collapsed: bool`. The sidebar remains unchanged internally and is conditionally omitted from the workspace layout when collapsed.

An icon-only gpui-component button is rendered by `WorkspaceView` as a window-level overlay at a fixed position beside the traffic lights. It remains visible in both states:

- expanded: show `IconName::PanelLeftClose` and the tooltip `Collapse sidebar`;
- collapsed: show `IconName::PanelLeftOpen` and the tooltip `Show sidebar`.

Clicking the button flips `sidebar_collapsed` and notifies GPUI to redraw. No controller action, persistence field, global state, animation, resizable rail, or replacement sidebar component is added.

The sidebar header receives enough top padding to reserve the titlebar row. New Task and Search remain full-width rows below it, preserving their existing actions and input state.

## Behavior

- Collapsing removes the sidebar completely and lets the chat area consume the released width.
- The restore control remains visible in the main header while collapsed.
- Expanding restores the same `SidebarView` entity, including its search text and current scroll/component state.
- A fresh launch starts expanded.

## Validation

- Run `cargo test -p threadlane-gpui`.
- Run `cargo check -p threadlane-gpui` and `git diff --check`.
- In a live GPUI run, verify collapse, restore, retained search state, New Task spacing, sidebar actions, window resizing, and tooltip/icon changes.
