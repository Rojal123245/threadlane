# Chat File Links in Editor Design

## Summary

Open project-relative file links from rendered chat Markdown in Threadlane's existing editor panel instead of passing them to macOS as external URLs. Keep web links opening in the default browser. This first version opens files only and does not navigate to line or column references.

## Problem

`TextView` uses `cx.open_url` as its default Markdown link-click behavior. That is correct for web links, but chat responses also contain project-relative links such as `docs/superpowers/specs/example.md`. Passing those paths to macOS produces an application-open error instead of opening the file in Threadlane.

The required editor path already exists: `ChatListView` owns an `EditorView`, observes `AppState::requested_editor_target`, switches to the Editor tab, and calls `EditorView::open_file` for file requests.

## Goals

- Open project-relative Markdown links from chat in the existing Editor tab.
- Preserve browser opening for `http://` and `https://` links.
- Apply identical behavior to rendered user and assistant messages.
- Reject absolute paths and paths that escape the active project.
- Report missing or unreadable files through the existing editor status UI rather than a macOS dialog.
- Reuse the existing editor request and tab-selection flow.

## Non-Goals

- Navigating to a line or column.
- Parsing `path:line`, `path#Lline`, or editor-specific link formats.
- Opening directories.
- Previewing files in a new component.
- Supporting files outside the active project.
- Introducing a custom URL scheme.

## Design

### Link classification

Add a small, testable link-classification helper near the chat renderer.

- Links beginning with `http://` or `https://` are external web links.
- Other links are candidates for project-relative files.
- File candidates must be relative and must not contain path traversal that escapes the project root.
- Fragments, line references, and column references are not interpreted in this version; the link text is treated as a file path only.

The implementation should use standard-library path handling and should not add a dependency.

### Click handling

Attach `TextView::on_link_click` to the Markdown `TextView` instances used for both user and assistant message content.

For a normal activation:

1. If the target is an external web link, call `cx.open_url` and preserve current behavior.
2. If the target is a valid project-relative file path, update `AppState` through `request_open_file` and notify observers.
3. `ChatListView`'s existing model observer switches `current_tab` to `CentralTab::Editor` and forwards the path to `EditorView::open_file`.
4. `EditorView` selects an existing tab for that path or opens a new tab.

Right-click and other non-activation events should not open either destination. The handler should preserve the component's current left-click, middle-click, keyboard, and non-long-press touch activation semantics where practical.

### Errors and safety

Absolute paths and project-escaping paths are not sent to the operating system. They are ignored or surfaced through a concise in-app status/notification using an existing UI mechanism.

Valid relative paths continue through `EditorView::open_file`. If a file is missing, unreadable, or not text, the editor's existing read failure handling reports the problem without invoking macOS.

## Testing

Add focused unit tests for the extracted classification/path helper:

- `docs/spec.md` is classified as a local project file.
- A nested relative path remains local.
- `https://example.com` and `http://example.com` remain external.
- An absolute path is rejected.
- A parent traversal that escapes the project is rejected.
- No line or fragment parsing occurs.

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests
cargo check -p threadlane-gpui
git diff --check
```

## Success Criteria

- Clicking the project-relative Markdown link shown in chat opens that file in Threadlane's Editor tab.
- Clicking a web link still opens the default browser.
- Invalid local paths never reach `cx.open_url`.
- Existing open-file tab reuse and editor errors remain intact.
- The change adds no dependency and no parallel editor state path.
