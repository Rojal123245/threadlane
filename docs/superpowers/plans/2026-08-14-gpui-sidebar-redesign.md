# GPUI Sidebar Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restyle Threadlane's GPUI sidebar to match the approved dark compact reference without changing its behavior.

**Architecture:** Keep the existing `SidebarView` and all state/controller wiring intact. Change only its GPUI render composition and styling, reusing gpui-component buttons, input, icons, and the active application theme.

**Tech Stack:** Rust, GPUI, gpui-component

## Global Constraints

- Modify only `crates/threadlane-gpui/src/screens/sidebar/view.rs` unless compilation proves a directly required adjustment elsewhere.
- Preserve `CreateSession`, search filtering, `AttachProject`, `SelectSession`, `ToggleSettings`, grouping, sorting, and status derivation.
- Add no dependency, state path, persistence change, or new reusable component abstraction.
- Preserve unrelated user changes in the dirty worktree.

---

### Task 1: Restyle the Existing Sidebar

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/sidebar/view.rs:21-430`

**Interfaces:**
- Consumes: `Entity<AppState>`, `Entity<InputState>`, `controller::dispatch`, `AppAction`, `SessionInfo`, `SessionHealth`, `ActiveTheme`, gpui-component `Button`, `Input`, and `IconName`.
- Produces: the existing `SidebarView` `Render` implementation with unchanged public construction and action behavior.

- [ ] **Step 1: Capture the compile baseline**

Run:

```bash
cargo check -p threadlane-gpui
```

Expected: PASS, or record any pre-existing failure before editing so it is not attributed to this task.

- [ ] **Step 2: Reshape the header without changing actions**

In `render_header`, remove the bordered light-style header treatment and render two compact ghost rows on the sidebar background:

```rust
div()
    .flex()
    .flex_col()
    .gap_1()
    .px_3()
    .pt_4()
    .pb_3()
```

Keep `new-task-btn` dispatching `AppAction::CreateSession` and retain `IconName::Plus`, the installed library's closest available compose symbol. Keep the search `InputState`, `InputEvent::Change` subscription, and appearance-free `Input`, presented as an icon-led row rather than a filled input box. Both rows must use theme colors and compact left alignment.

- [ ] **Step 3: Match the target list heading and attach action**

In `render_history_header`, change `Recent Tasks` to `Today`, remove the text `+`, and use `IconName::Folder` for the attach-project action. Keep the same `rfd::FileDialog` and `AppAction::AttachProject(path)` closure. Style the heading and icon as muted, borderless controls aligned to the same horizontal inset as the cards.

Change `DateGroup::label` values to title case:

```rust
match self {
    Self::Today => "Today",
    Self::Yesterday => "Yesterday",
    Self::ThisWeek => "This Week",
    Self::Older => "Older",
}
```

Avoid rendering a duplicate `Today` group label inside `render_history`: the fixed heading owns the Today label; render labels there only for later non-empty groups.

- [ ] **Step 4: Restyle session cards while preserving selection and data**

Keep the existing unique card ID and `AppAction::SelectSession { work_dir, session_id }` closure. Reshape each card into a rounded two-line surface:

```rust
div()
    .id(SharedString::from(format!("session-card-{}", session.id)))
    .mx_2()
    .my_0p5()
    .px_3()
    .py_2()
    .rounded_lg()
```

The first line contains a larger semibold, truncated title and the working/health indicator aligned right. The second line contains a folder icon, truncated project directory name, and right-aligned status text: `Working for {elapsed}` for `SessionHealth::Working`, otherwise the existing elapsed time. Use only existing theme roles for inactive, hover, active, foreground, muted, success, warning, and working colors. Keep sorting, filtering, grouping, and active-session comparison unchanged.

- [ ] **Step 5: Pin an icon-only settings action to the bottom**

In `render_footer`, retain `Button::new("sidebar-settings")`, `IconName::Settings`, and `AppAction::ToggleSettings`. Remove the `Settings` label, full-width presentation, and top divider; use a compact ghost icon button aligned to the lower-left with `.tooltip("Settings")`.

Keep the outer sidebar as a fixed-width, full-height flex column. Preserve the scrollable history as `.flex_1().min_h_0().overflow_y_scrollbar()` so the footer remains pinned.

- [ ] **Step 6: Format and compile**

Run:

```bash
cargo fmt -- crates/threadlane-gpui/src/screens/sidebar/view.rs
cargo check -p threadlane-gpui
git diff --check
```

Expected: all commands PASS.

- [ ] **Step 7: Perform a fresh visual interaction check**

Launch the GPUI binary:

```bash
cargo run -p threadlane-gpui
```

Verify against the approved dark reference:

- New Task creates a task.
- Search filters and its empty state remains readable.
- Attach Project opens the folder chooser.
- Normal, hover, active, healthy, warning, and working cards remain legible.
- Session selection still switches sessions.
- Settings opens from the bottom icon.
- Long titles and project names truncate without shifting the status indicator.
- Scrolling leaves Settings pinned.

- [ ] **Step 8: Commit only the sidebar implementation**

```bash
git add crates/threadlane-gpui/src/screens/sidebar/view.rs
git commit -m "feat(gpui): refresh sidebar layout"
```
