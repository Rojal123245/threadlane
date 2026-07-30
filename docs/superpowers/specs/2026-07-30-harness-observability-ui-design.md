# Harness Observability UI Design

## Goal

Expose the completed harness lifecycle in Threadlane’s existing chat and session UI without turning the product into a debugging console.

## Decision

Use existing chat activity rows as the primary observability surface. Add a restrained session-sidebar badge only when a session has unresolved recovery or aborted work. Do not add a dedicated harness panel, raw operation-log viewer, replay controls, or new user-facing task-management workflow in this slice.

## User experience

Normal chat remains visually dominant. Successful background work is quiet and summarized; recovery and unsafe interruption are visible but concise.

Chat activity summaries use the existing grouped activity presentation:

- `Delegated · 2 tasks`
- `Recovering · 1 task`
- `Recovered · 1 task`
- `Aborted · unsafe tool`
- `Retrying recovery`

Expanded activity content shows only useful human-facing details:

- task name or short task prompt;
- lane state;
- concise tool summary;
- recovery or abort reason when relevant.

Do not show raw operation IDs, sequence numbers, JSON records, or internal replay classifications in the normal UI.

The session sidebar remains unchanged for healthy sessions. A muted warning/recovery badge appears only for unresolved recovery or aborted work. The badge is session-scoped and does not compete with the selected-session treatment.

## Scope

### In scope

- Map harness lifecycle events into the existing `GuiAgentEvent` path.
- Render subagent delegation and lane outcomes through the existing chat activity grouping.
- Render recovery progress, retryable recovery failure, safe recovery completion, and unsafe abort status.
- Render a session-level unresolved-status badge in the existing session/sidebar row.
- Keep detached background task state and foreground subagent state sourced from the canonical supervisor/session registries.
- Add deterministic presentation tests for lifecycle-to-display-row mapping and duplicate suppression.

### Out of scope

- A new harness dashboard or lane browser.
- Raw oplog inspection.
- Per-tool replay or retry controls.
- New persistence or event transport.
- Partial token-delta visualization for subagents.
- Changing session-tree semantics or recovery behavior.

## Architecture

The backend remains the source of truth. `HarnessSupervisor`, `CodingAgent`, and `SessionTree` own lifecycle state; the UI observes them through the existing `GuiAgentEvent` and session-state synchronization paths.

The UI should introduce one presentation-level status type, kept near the existing chat display-row construction, rather than duplicating supervisor state in widgets. The mapper consumes lifecycle events and persisted lane/session metadata and produces concise activity summaries plus an optional session health marker.

The data flow is:

```text
HarnessSupervisor / CodingAgent / SessionTree
        -> GuiAgentEvent + canonical session state
        -> app event handling and session synchronization
        -> chat display-row mapper
        -> existing grouped activity row
        -> optional sidebar health badge
```

Live and persisted lane events must share a stable presentation key based on the durable lane/run identity. If a persisted recovery event and a live completion describe the same lane, the mapper updates one row instead of appending a duplicate.

## State mapping

| Harness state | Chat presentation | Sidebar badge |
|---|---|---|
| queued/started | `Delegated` / `Working` activity | none |
| running tools | existing grouped tool activity | none |
| recovery started | `Recovering` activity | muted recovery while unresolved |
| safe recovery completed | `Recovered` activity, collapsed by default | none |
| retryable recovery error | `Retrying recovery` or concise recovery error | warning |
| unsafe interruption | `Aborted · unsafe tool` | warning |
| cancellation completed | `Cancelled` activity when user-visible | none unless work remains unresolved |

The mapper must prefer the latest terminal state for a lane, preserve the underlying chat/session records, and avoid emitting a second row when an activity group already represents the same lane.

## Interaction and accessibility

- Activity rows remain collapsible using the existing `Working`/`Worked` interaction.
- Successful recovery does not steal focus or open a popup.
- Recovery errors use existing error styling and concise copy; they do not block unrelated session navigation.
- Badge color and text must not be the only status signal; use a short accessible label or tooltip.
- Keyboard navigation and existing activity-row focus behavior remain unchanged.

## Error behavior

Recovery progress is transient and should disappear or collapse into the terminal summary when complete. A retryable failure remains visible until the next successful recovery attempt or the user leaves the session. Unsafe aborts remain in the activity history as a terminal outcome.

If lifecycle events arrive out of order, the mapper uses the durable lane key and terminal-state precedence rather than rendering contradictory duplicate rows. Missing optional task text falls back to a concise generic label such as `Subagent task`.

## Testing and verification

- Unit-test the lifecycle-to-display-row mapper for queued, running, recovering, recovered, retrying, aborted, and cancelled states.
- Test that live and persisted events for one durable lane produce one display row.
- Test that terminal status replaces an in-progress summary without losing the underlying message history.
- Test sidebar badge visibility for healthy, retryable-error, and unsafe-abort sessions.
- Run `cargo test -p threadlane` and focused chat/session tests.
- Run `cargo check -p threadlane` and `git diff --check`.
- Perform runtime visual verification for activity-row expansion, badge placement, and keyboard focus; compilation alone is insufficient for these UI changes.

## Minimalism guardrails

- Reuse the existing activity group, session row, theme tokens, and event transport.
- Add no new panel, persistence record, dependency, or global status store.
- Keep summaries to one short line and details bounded when expanded.
- Hide implementation identifiers from normal users.
- Prefer one mapper and one small badge variant over per-event widget logic.
