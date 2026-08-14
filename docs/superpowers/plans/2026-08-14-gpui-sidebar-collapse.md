# GPUI Sidebar Collapse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a persistent titlebar control that completely hides and restores the GPUI sidebar while leaving New Task on its own row.

**Architecture:** `WorkspaceView` owns the transient collapsed state, conditionally renders its existing `SidebarView` entity, and paints one icon-only overlay button in both states. `ChatListView` receives only a derived header inset so its title clears the traffic lights and restore control while collapsed.

**Tech Stack:** Rust, GPUI, gpui-component

## Global Constraints

- Start expanded and do not persist collapse state.
- Preserve the existing `SidebarView` entity so search and component state survive collapse/restore.
- Add no controller action, global state, dependency, animation, rail, or replacement sidebar component.
- Preserve unrelated user changes in the dirty worktree.

---

### Task 1: Add the Sidebar Visibility Toggle

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/workspace/view.rs:1-58`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:8-130`
- Modify: `crates/threadlane-gpui/src/screens/sidebar/view.rs:98-109`

**Interfaces:**
- Consumes: existing `Entity<SidebarView>`, `Entity<ChatListView>`, gpui-component `Button`, `ButtonVariants`, `IconName::PanelLeftClose`, and `IconName::PanelLeftOpen`.
- Produces: `WorkspaceView::sidebar_collapsed: bool` and `ChatListView::header_left_padding: Pixels`; public constructors remain unchanged.

- [ ] **Step 1: Capture the package baseline**

Run:

```bash
cargo test -p threadlane-gpui
cargo check -p threadlane-gpui
```

Expected: both commands pass; record any pre-existing warnings separately.

- [ ] **Step 2: Reserve the sidebar titlebar row**

In `SidebarView::render_header`, replace `.pt_4()` with an explicit titlebar inset:

```rust
.pt(px(48.0))
```

Keep New Task and Search unchanged below that inset.

- [ ] **Step 3: Let the chat header clear collapsed window controls**

Add a derived layout field to `ChatListView` and initialize it for the normal expanded layout:

```rust
pub header_left_padding: Pixels,
```

```rust
header_left_padding: px(16.0),
```

In `render_header`, replace `.px_4()` with:

```rust
.pl(self.header_left_padding)
.pr_4()
```

No chat behavior or model state changes.

- [ ] **Step 4: Own and toggle collapsed state in the workspace**

Import the existing gpui-component controls in `workspace/view.rs`:

```rust
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::IconName;
```

Add and initialize:

```rust
sidebar_collapsed: bool,
```

```rust
sidebar_collapsed: false,
```

At the start of `render`, derive the icon and tooltip:

```rust
let sidebar_icon = if self.sidebar_collapsed {
    IconName::PanelLeftOpen
} else {
    IconName::PanelLeftClose
};
let sidebar_tooltip = if self.sidebar_collapsed {
    "Show sidebar"
} else {
    "Collapse sidebar"
};
```

Conditionally render the existing sidebar entity:

```rust
.children((!self.sidebar_collapsed).then(|| self.sidebar.clone()))
```

Render this button after the chat child and before the settings modal so it paints over normal content but beneath the modal:

```rust
.child(
    Button::new("sidebar-collapse-toggle")
        .icon(sidebar_icon)
        .tooltip(sidebar_tooltip)
        .ghost()
        .absolute()
        .top(px(10.0))
        .left(px(88.0))
        .on_click(cx.listener(|this, _event, _window, cx| {
            this.sidebar_collapsed = !this.sidebar_collapsed;
            let inset = if this.sidebar_collapsed { px(128.0) } else { px(16.0) };
            this.chat_list.update(cx, |chat, cx| {
                chat.header_left_padding = inset;
                cx.notify();
            });
            cx.notify();
        })),
)
```

- [ ] **Step 5: Format and verify**

Run:

```bash
cargo fmt -- crates/threadlane-gpui/src/screens/workspace/view.rs crates/threadlane-gpui/src/screens/chat/view.rs crates/threadlane-gpui/src/screens/sidebar/view.rs
cargo test -p threadlane-gpui
cargo check -p threadlane-gpui
git diff --check
```

Expected: all commands pass.

- [ ] **Step 6: Verify the live interaction**

Run:

```bash
cargo run -p threadlane-gpui
```

Verify the panel icon sits beside the traffic lights, New Task starts below the titlebar, collapse removes the sidebar completely, the chat title clears the restore control, restore returns the same search text and scroll/component state, resizing remains stable, and both tooltips/icons match their state.
