# Session Todo Plans Design

## Goal

Give each Threadlane session one durable model-managed plan whose progress is
visible beside the project-wide task list. Plans are planning state, not
executable supervisor tasks.

## Scope

The first version provides:

- One ordered plan per session.
- Model updates through a host `update_plan` tool.
- `pending`, `in_progress`, and `completed` item states.
- Persistence in the existing session JSONL.
- Immediate UI updates during a generation.
- Restoration when a session is reopened.
- Current-session plan progress and items in the existing right sidebar.

It does not add slash commands, manual editing, plan history UI, due dates,
assignees, or supervisor integration.

Completed items remain visible until the model replaces the plan. Submitting an
empty plan clears it.

## Data Model

Define provider-neutral plan types in `threadlane-agent`:

```rust
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

pub struct PlanItem {
    pub step: String,
    pub status: PlanItemStatus,
}

pub struct SessionPlan {
    pub explanation: Option<String>,
    pub items: Vec<PlanItem>,
}
```

Plan equality is structural so UI synchronization can skip unchanged updates.
Input is bounded to 20 items, 200 characters per step, and 500 characters for
the optional explanation. Steps must be non-empty and at most one item may be
`in_progress`.

## Persistence

Add a dedicated tagged record to the existing session JSONL:

```json
{
  "type": "session_plan",
  "explanation": "Implement the feature in dependency order.",
  "items": [
    {"step": "Add persistence", "status": "completed"},
    {"step": "Register the tool", "status": "in_progress"},
    {"step": "Render the sidebar", "status": "pending"}
  ]
}
```

`SessionTree::load_from_file` applies the latest plan record. A successful plan
update appends a new plan record while holding the same process-wide session
file lock used by message and metadata writes. Full transactional rewrites also
write the current plan, so title/model updates preserve it.

Using a separate record instead of deriving the plan from tool-call history
keeps the plan intact when conversation context is compacted. It also avoids a
new file or persistence subsystem.

Unsaved project drafts keep plan state in memory. The normal first-send path
creates a session-backed `CodingAgent`; subsequent plan updates then persist to
that session file.

## Model Tool

Register one reserved host executor in `CodingAgent`:

```text
update_plan({
  explanation?: string,
  plan: [{step: string, status: "pending" | "in_progress" | "completed"}]
})
```

The executor:

1. Parses and validates the complete replacement plan.
2. Persists it through the session plan store.
3. Emits `AgentEvent::PlanUpdated { plan }`.
4. Returns a concise success result containing completed and total counts.

Invalid updates return tool errors without changing memory, disk, or UI state.
The tool is available only to the parent `CodingAgent`; model-spawned child
agents do not own the parent session plan.

The system prompt describes when to use the tool: use it for multi-step work,
keep exactly one active step where practical, and update statuses as work
progresses. Simple requests do not require a plan.

## Runtime State and Events

`CodingAgent` owns a small shared session plan store containing the active plan
and optional session path. The tool executor and `CodingAgent::current_plan`
use the same store.

On construction, the store is initialized from `SessionTree`. The app initializes
each `SessionRuntime` from `CodingAgent::current_plan`. During generation,
`PlanUpdated` replaces that runtime's plan and refreshes the sidebar only when
the event belongs to the current generation.

Each session runtime therefore retains its own plan while users switch between
sessions. Reopening a session reconstructs the runtime from the persisted
session plan record.

## Sidebar

Keep the existing project-scoped Tasks sidebar and add a distinct plan section
above its task list:

```text
Tasks                                      ×

PLAN · 2/4
✓ Inspect current behavior
● Implement persistence
○ Wire the sidebar
○ Verify runtime behavior

PROJECT TASKS
CURRENT CHAT
...
OTHER SESSION
...
```

The plan section always represents the current session only. Project tasks
remain grouped across sessions as they are today.

Presentation rules:

- Hide the plan section when the current session has no plan.
- Show completed/total progress.
- Use the existing subtle status colors and simple SVG/drawn indicators.
- Bound and ellipsize step text.
- Keep rows read-only in this version.
- The header task button is visible when either the project has task records or
  the current session has a plan.
- Its badge shows active executable task count; plan progress remains inside
  the sidebar to avoid mixing the two concepts.

The sidebar remains manually toggled and never opens automatically.

## Error Handling

- Reject malformed JSON, unknown statuses, empty steps, oversized text, more
  than 20 items, and multiple `in_progress` items.
- Do not partially apply invalid plans.
- Surface persistence failures as tool errors and retain the prior plan.
- Ignore stale plan events from superseded generations.
- Use guarded `PortalList` indexing because dynamic ranges can yield one stale
  item ID after shrinking.

## Testing

Add focused coverage for:

- Plan validation and complete replacement semantics.
- Empty-plan clearing.
- Session JSONL round trips and legacy-session compatibility.
- Transactional metadata rewrites preserving plans.
- Tool registration, success, validation errors, emitted events, and
  persistence failure rollback.
- Runtime initialization and per-session isolation.
- Sidebar row construction, progress counts, hidden-empty behavior, and stale
  portal row safety.

Validation:

```bash
cargo test -p threadlane-agent
cargo test -p threadlane-coding-agent
cargo test -p threadlane
cargo check -p threadlane
just hawkcheck
git diff --check
```

Use a fresh Makepad Studio run to verify plan visibility, progress styling,
session switching, task coexistence, clearing, and sidebar resizing.

