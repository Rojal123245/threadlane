# Project Header Hover Ownership

## Goal

Show the detach and new-session controls whenever the pointer is over a project header, hide them immediately on pointer exit, and keep the session list layout stable.

## Design

Replace app-level pointer tracking for project-row actions with a reusable `ProjectHeader` widget in `components/session_row.rs`.

The widget will:

- dereference and draw its existing `View`;
- delegate events to its child controls;
- handle hover-in and hover-out against its own area;
- update and redraw its action buttons locally;
- emit typed detach, new-session, and select-project actions.

Both fixed and portal-list project headers will use the same widget. The app and session-list shell will consume its typed actions and will no longer calculate project hover from retained global pointer coordinates.

## Verification

- Launch `cargo run -p threadlane`.
- Move repeatedly between the project name, both action buttons, session rows, and outside the sidebar; controls must appear and disappear without clicking or shifting layout.
- Run `cargo check -p threadlane`, focused session tests, Clippy with `-D warnings`, and `git diff --check`.
