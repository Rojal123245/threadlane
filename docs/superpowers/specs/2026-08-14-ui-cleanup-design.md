# Threadlane UI Cleanup Design

## Goal

Reduce dead UI code, duplicated Makepad templates, repeated state synchronization,
and inconsistent interaction styling without changing product behavior or adding
dependencies.

## Phases

### 1. Delete unused UI scaffolding

Remove dead panel state/view modules and unreferenced component templates. Remove
their module declarations and script registration in the same change. Retain
small wrappers that encode behavior or are used by more than one call site.

### 2. Simplify chat presentation

Keep the restored individual `ThinkingMsg` and `ToolMsg` activity rows and the
existing `SubagentRail`. Consolidate repeated jump-to-latest synchronization and
reuse the existing fold-row base where Makepad prototype rules permit it. Do not
change transcript ordering, grouping, persistence, or auto-tail behavior.

### 3. Consolidate settings presentation

Move settings-modal structure toward its owning component, table-drive page and
navigation visibility, and reuse one empty-row/status presentation. Preserve all
page IDs, portal-list behavior, authentication actions, and settings semantics.

### 4. Normalize visual state

Replace component-local color literals with existing semantic theme roles and
complete hover, focus, pressed, border, loading, and disabled states for controls
in the touched views. Preserve the current dark visual language and picker popup
geometry.

### 5. Visual density and accessibility pass

Use fresh Makepad Studio runs to inspect transcript rows, settings, sidebars,
dialogs, and narrow-window layouts. Normalize only equivalent spacing and add
keyboard parity where custom clickable views currently depend on pointers.

## Constraints

- No new dependencies or UI framework.
- No changes to persistence, provider routing, task lifecycle, or session state.
- No speculative component abstractions.
- Preserve the user's existing uncommitted harness and activity-rail changes.
- Each phase must compile, pass focused tests, pass `git diff --check`, and receive
  a fresh Studio run for runtime-only Makepad behavior when Studio is available.

## Validation

- `cargo check -p threadlane`
- focused tests for each touched component
- `cargo test --workspace -- --test-threads=1`
- `git diff --check`
- fresh Makepad Studio widget-tree and screenshot checks for visual phases
