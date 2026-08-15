# GPUI Sidebar Redesign

## Goal

Restyle the existing GPUI sidebar to match the supplied dark compact reference while preserving all current behavior.

## Scope

Change only the presentation in `crates/threadlane-gpui/src/screens/sidebar/view.rs`. Reuse the existing `AppState`, controller actions, `InputState`, date grouping, search filtering, project attachment, session selection, health state, and settings toggle.

The sidebar will contain:

- a compact top navigation area with icon-led New Task and Search controls;
- title-case date group headings with the existing attach-project action beside the relevant list heading;
- rounded session cards showing title, project folder, elapsed or working status, and health state;
- an icon-only Settings action pinned to the bottom;
- dark surfaces, muted secondary text, subtle hover states, and a stronger active-session surface matching the reference.

Use gpui-component `Button`, `Input`, and `IconName` controls plus the existing application theme. Do not add dependencies, state paths, or reusable component abstractions.

## Behavior

All existing interactions remain routed through their current actions:

- New Task dispatches `CreateSession`.
- Search updates `AppState::search_query` and filters the existing session collection.
- Attach Project opens the existing folder picker and dispatches `AttachProject`.
- Session cards dispatch `SelectSession`.
- Settings dispatches `ToggleSettings`.

Date grouping, sorting, active-session selection, and status derivation remain unchanged. The redesign must not alter persistence or session data.

## Validation

- Add no new behavioral test for presentation-only styling; retain existing logic tests.
- Run `cargo check -p threadlane-gpui`.
- Run `git diff --check`.
- Launch the GPUI application and compare the sidebar visually against the supplied target, checking normal, hover, active, working, filtered, and empty-list states.
