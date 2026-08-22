# Chat File Links in Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open safe project-relative links from user and assistant chat Markdown in Threadlane's existing editor panel while preserving browser behavior for HTTP(S) links.

**Architecture:** Add one pure link-classification helper and one `ChatListView` Markdown-view builder in the existing chat view module. The builder overrides `TextView`'s default link handler, sends HTTP(S) links to `cx.open_url`, and sends validated relative paths through the existing `AppState::request_open_file` observer flow; the existing editor remains responsible for tab reuse and read errors.

**Tech Stack:** Rust 2024, GPUI, `gpui-component::text::TextView`, standard-library `std::path::{Component, Path, PathBuf}`.

## Global Constraints

- This first version opens files only and does not navigate to line or column references.
- Apply identical behavior to rendered user and assistant message content; reasoning-block Markdown is outside this scope.
- Preserve browser opening for links beginning with `http://` or `https://`.
- Reject absolute paths and relative paths whose `..` components would escape the active project.
- Treat fragments and line-like suffixes as literal path text; do not parse `path:line` or `path#Lline`.
- Add no dependency, custom URL scheme, preview component, or parallel editor state path.
- Preserve the pre-existing uncommitted changes in `crates/threadlane-gpui/src/screens/chat/view.rs` and `crates/threadlane-gpui/src/state/app_state.rs`; stage only this feature's hunks when committing.

## File Structure

- Modify `crates/threadlane-gpui/src/screens/chat/view.rs`: own link classification, click routing, user/assistant renderer hookup, and focused unit tests because all behavior is local to rendered chat Markdown.
- Do not modify `crates/threadlane-gpui/src/state/app_state.rs`: consume the existing `AppState::request_open_file(String)` API.
- Do not modify `crates/threadlane-gpui/src/screens/editor/view.rs`: consume the existing `EditorView::open_file(&str, ...)` behavior, including tab reuse and in-editor read errors.

---

### Task 1: Route Chat Markdown Links

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:1-115`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:2064-2096`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:2211-2285`
- Test: `crates/threadlane-gpui/src/screens/chat/view.rs:3477-3526` (`hot_path_tests`)

**Interfaces:**
- Consumes: `AppState::request_open_file(relative_path: String)` in `crates/threadlane-gpui/src/state/app_state.rs`.
- Consumes: the existing `ChatListView` model observer that takes `RequestedEditorTarget::File`, selects `CentralTab::Editor`, and calls `EditorView::open_file`.
- Produces: `enum ChatLinkTarget { Web, ProjectFile(String), Rejected }`.
- Produces: `fn classify_chat_link(link: &str) -> ChatLinkTarget`.
- Produces: `fn chat_markdown_view(&self, state: &Entity<TextViewState>) -> TextView`.

- [ ] **Step 1: Add focused failing classifier tests**

Extend the `hot_path_tests` import list with `classify_chat_link` and `ChatLinkTarget`, then add these tests near the existing Markdown tests:

```rust
#[test]
fn chat_link_classifies_web_urls_as_external() {
    assert_eq!(
        classify_chat_link("https://example.com/spec"),
        ChatLinkTarget::Web
    );
    assert_eq!(
        classify_chat_link("http://example.com/spec"),
        ChatLinkTarget::Web
    );
}

#[test]
fn chat_link_normalizes_safe_project_relative_paths() {
    assert_eq!(
        classify_chat_link("docs/spec.md"),
        ChatLinkTarget::ProjectFile("docs/spec.md".into())
    );
    assert_eq!(
        classify_chat_link("docs/design/../spec.md"),
        ChatLinkTarget::ProjectFile("docs/spec.md".into())
    );
}

#[test]
fn chat_link_rejects_absolute_and_escaping_paths() {
    assert_eq!(classify_chat_link("/tmp/spec.md"), ChatLinkTarget::Rejected);
    assert_eq!(
        classify_chat_link("../../outside.md"),
        ChatLinkTarget::Rejected
    );
}

#[test]
fn chat_link_does_not_parse_line_or_fragment_suffixes() {
    assert_eq!(
        classify_chat_link("src/main.rs:42"),
        ChatLinkTarget::ProjectFile("src/main.rs:42".into())
    );
    assert_eq!(
        classify_chat_link("src/main.rs#L42"),
        ChatLinkTarget::ProjectFile("src/main.rs#L42".into())
    );
}
```

- [ ] **Step 2: Run the focused tests and verify the red state**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests::chat_link -- --nocapture
```

Expected: compilation fails because `classify_chat_link` and `ChatLinkTarget` do not exist yet. Confirm the failure is limited to those missing symbols before implementing.

- [ ] **Step 3: Implement the minimal pure classifier**

Add `Path` and `PathBuf` to the standard-library imports, then add this helper alongside `classify_markdown_update`:

```rust
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
enum ChatLinkTarget {
    Web,
    ProjectFile(String),
    Rejected,
}

fn classify_chat_link(link: &str) -> ChatLinkTarget {
    if link.starts_with("http://") || link.starts_with("https://") {
        return ChatLinkTarget::Web;
    }

    let mut normalized = PathBuf::new();
    for component in Path::new(link).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return ChatLinkTarget::Rejected;
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        ChatLinkTarget::Rejected
    } else {
        ChatLinkTarget::ProjectFile(normalized.to_string_lossy().into_owned())
    }
}
```

This lexical normalization permits an internal `docs/design/../spec.md` but rejects a `..` after the normalized path has reached the project root. It does not touch the filesystem and therefore cannot accidentally canonicalize or open a path outside the active project.

- [ ] **Step 4: Run the classifier tests and verify the green state**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests::chat_link -- --nocapture
```

Expected: all four `chat_link_*` tests pass.

- [ ] **Step 5: Add the shared Markdown link handler**

Add this method directly after `ChatListView::markdown_state`:

```rust
fn chat_markdown_view(&self, state: &Entity<TextViewState>) -> TextView {
    let model = self.model.clone();
    TextView::new(state)
        .selectable(true)
        .on_link_click(move |url, event, _window, cx| {
            let activate = match event {
                ClickEvent::Mouse(click) => {
                    matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
                }
                ClickEvent::Keyboard(_) => true,
                ClickEvent::Touch(click) => !click.long_press,
            };
            if !activate {
                return;
            }

            match classify_chat_link(url) {
                ChatLinkTarget::Web => cx.open_url(url),
                ChatLinkTarget::ProjectFile(path) => {
                    model.update(cx, |state, cx| {
                        state.request_open_file(path);
                        cx.notify();
                    });
                }
                ChatLinkTarget::Rejected => {}
            }
        })
}
```

The explicit `ClickEvent` match copies `TextView`'s built-in activation policy. Invalid local targets are intentionally ignored, so they never reach macOS; valid but missing/unreadable relative files continue into the editor's existing status-message path.

- [ ] **Step 6: Hook both user and assistant message Markdown into the handler**

In `render_message`, replace only the two `TextView::new(&markdown_state).selectable(true)` expressions used for user message content and assistant message content:

```rust
self.chat_markdown_view(&markdown_state)
```

Do not replace the reasoning-block `TextView` in `render_reasoning_block`; reasoning links were not approved for this first version. Do not alter the context menus, streaming animation, or Markdown state caching.

- [ ] **Step 7: Run focused and package validation**

Run these commands in order:

```bash
cargo test -p threadlane-gpui hot_path_tests::chat_link -- --nocapture
cargo test -p threadlane-gpui hot_path_tests
cargo check -p threadlane-gpui
git diff --check
```

Expected:

- All four `chat_link_*` tests pass.
- All `hot_path_tests` pass.
- `cargo check -p threadlane-gpui` exits successfully; pre-existing dead-code warnings are acceptable.
- `git diff --check` exits with no whitespace errors.

- [ ] **Step 8: Review the diff without disturbing user work**

Run:

```bash
git diff -- crates/threadlane-gpui/src/screens/chat/view.rs
git status --short
```

Confirm the feature diff contains only the import, classifier, shared handler, two message-renderer replacements, and focused tests. Confirm `crates/threadlane-gpui/src/state/app_state.rs` remains modified only by the pre-existing user work and was not edited for this feature.

- [ ] **Step 9: Commit only the feature hunks**

Because `chat/view.rs` already contains unrelated uncommitted changes, interactively stage only this task's hunks:

```bash
git add -p crates/threadlane-gpui/src/screens/chat/view.rs
git diff --cached --check
git diff --cached
```

Verify the staged diff excludes the pre-existing system-message rendering changes and excludes all of `crates/threadlane-gpui/src/state/app_state.rs`. Then commit:

```bash
git commit -m "feat(gpui): open chat file links in editor"
```

Expected: one commit containing only the chat-file-link implementation and tests; unrelated working-tree changes remain unstaged.

## Manual Acceptance Check

After the automated checks pass, run:

```bash
cargo run -p threadlane-gpui
```

In a project chat, render or locate a message containing both `[spec](docs/superpowers/specs/2026-08-22-chat-file-links-editor-design.md)` and `[web](https://example.com)`.

Confirm:

1. Clicking `spec` switches the central area to the Editor tab and opens/reuses the file tab.
2. Clicking `web` opens the default browser.
3. Clicking a link such as `[outside](../../outside.md)` does nothing and produces no macOS “application can’t be opened” dialog.
4. A valid relative link to a missing file switches to the editor and shows the editor's existing “Unable to open …” status.

Do not claim this visual behavior was verified unless the application was actually run and observed.
