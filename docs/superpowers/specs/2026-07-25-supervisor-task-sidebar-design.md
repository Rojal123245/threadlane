# Supervisor Task Sidebar Design

## Problem

Threadlane currently exposes two unrelated concepts:

- `/task <prompt>` creates a detached `CodingAgent` through `HarnessSupervisor`.
- The model-facing `subagent` tool runs child agents inside the active `CodingAgent`.

`HarnessSupervisor` tracks only the first path. The desktop app reduces that
state to a `"N tasks · M running"` header button, and clicking it writes another
summary line into chat. Model-spawned subagents are visible only inside their
parent tool row. Consequently the supervisor is not a useful place to inspect
or control all delegated work, and the empty `"0 tasks"` button permanently
occupies header space.

oh-my-pi keeps execution and presentation separate but normalizes child-agent
lifecycle and progress into records containing a stable identity, parent
relationship, assignment, state, current activity, and owning session. That is
the pattern Threadlane should adopt without replacing its working execution
paths.

## Goals

- Track detached `/task` agents and model-spawned `subagent` workers in one
  supervisor-owned registry.
- Scope the visible registry to the active project and group rows by owning chat
  session.
- Show queued, running, completed, failed, and cancelled work with the agent
  name, assignment, and current tool/activity.
- Let users open the owning session from a task row.
- Let users cancel a running detached task when a cancellation handle exists.
- Replace the current summary behavior with a collapsible right sidebar.
- Hide the task control entirely while the active project has no tracked work.
- Preserve the existing inline subagent activity rail and chat transcript as the
  detailed record of work.

## Non-goals

- Do not route model-spawned subagents through `HarnessSupervisor`; their
  synchronous result contract remains unchanged.
- Do not add worktrees, nested subagent spawning, usage accounting, task
  persistence, or a second task-detail transcript.
- Do not add a dependency or a new storage format.
- Do not automatically open the sidebar when a task starts.

## Architecture

### Normalized lifecycle events

Add explicit subagent lifecycle events to `threadlane_agent::AgentEvent`:

```rust
SubagentQueued {
    run_id: u64,
    task_index: usize,
    agent: String,
    task: String,
},
SubagentStarted {
    run_id: u64,
    task_index: usize,
},
SubagentFinished {
    run_id: u64,
    task_index: usize,
    succeeded: bool,
    error: Option<String>,
},
```

`run_subagents_with_context` emits `SubagentQueued` for every requested worker,
`SubagentStarted` after that worker acquires its execution permit, and
`SubagentFinished` after it returns. Existing relayed child tool events retain
IDs in the form `subagent-{run_id}:{task_index}:{child_id}`. The explicit
lifecycle removes the current heuristic association between a parent subagent
call and its first child tool event.

### Supervisor registry

`HarnessSupervisor` remains the owner of execution handles for detached tasks
and becomes the canonical query store for all task records.

`TaskRecord` gains:

- `kind: TaskKind` (`Background` or `Subagent`)
- `session_id`
- `parent_task_id`
- `agent`
- `summary`
- `current_activity`
- `status`
- `started_at_ms`
- `finished_at_ms`

The app forwards foreground `GenerationAgent` events to
`HarnessSupervisor::observe_session_event`. Background-agent events pass through
the same reducer inside the supervisor's existing event-forwarding task.

The reducer:

- Inserts an idle/queued subagent task on `SubagentQueued`.
- Marks it running on `SubagentStarted`.
- Updates `current_activity` from prefixed child `ToolExecutionStart` events.
- Clears or replaces activity when the matching child tool ends.
- Settles the row on `SubagentFinished`.
- Continues updating the existing detached parent record from
  `AgentStart`/`AgentEnd`/`AgentError`.

No event payload or output is duplicated into the registry. Chat/session files
remain the authoritative detailed history.

### UI state and sidebar

Create a reusable `TaskSidebar` Makepad widget under `components/`. It owns a
scrollable row list and emits typed actions:

```rust
pub enum TaskSidebarAction {
    Close,
    OpenSession {
        session_id: String,
        session_file: Option<PathBuf>,
    },
    Cancel(String),
    None,
}
```

The app queries `HarnessSupervisor::list_tasks_for_project` whenever an observed
task event changes the registry or the active project/session changes. It gives
the resulting rows to `TaskSidebar`.

The existing workspace `content_row` remains a horizontal layout:

```text
chat_column (Fill) | task_sidebar (280 px, optional)
```

The panel groups tasks under session headers. Within each session, running and
queued work sorts before recent terminal work. A task row shows:

- State indicator
- Bounded assignment text
- Agent name and current activity
- Stop icon only for cancellable running detached tasks

Selecting a row resolves a stored session file through session discovery and
calls the existing `activate_session` path. A task owned by the unsaved project
draft selects that draft directly. The panel closes through its own header icon
but otherwise preserves its open state while switching sessions in the same
project.

### Header behavior

The current text-only `tasks_btn` becomes a compact icon button with the active
count. It is hidden when the active project has no task records. Clicking it
toggles the right sidebar. If the last task record disappears, the button and
panel both hide.

Completed rows remain visible for the current application run, so users can
inspect recently completed work. Registry persistence is deliberately deferred;
completed subagent details are already durable in session transcripts.

## Error and cancellation behavior

- A failed child agent produces a failed row with the returned error as its
  bounded activity text.
- Cancelling a detached task uses the existing `HarnessSupervisor::cancel_task`.
- Model-spawned subagents remain non-cancellable until their executor exposes a
  per-worker abort handle; their rows never show a misleading stop control.
- A task whose session file is not yet discoverable stays visible. Selecting it
  refreshes sessions once; if it still cannot be resolved, the app writes one
  concise system message rather than silently failing.

## Testing

- Unit-test lifecycle reduction in `supervisor.rs`, including parallel child
  identities, current-tool updates, completion, failure, and project filtering.
- Unit-test sidebar row grouping/sorting without requiring a Makepad runtime.
- Unit-test the empty/active header presentation decision.
- Run `cargo test -p threadlane-coding-agent supervisor`.
- Run focused `threadlane` state/component tests.
- Run `cargo check -p threadlane` and `git diff --check`.
- Use the Makepad Studio remote workflow to verify the optional right panel,
  empty header state, task-row interaction, and resizing behavior at runtime.

## Research references

- [oh-my-pi coding-agent architecture](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/DEVELOPMENT.md)
- [oh-my-pi task lifecycle and progress types](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/task/types.ts)
