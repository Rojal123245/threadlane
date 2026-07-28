# Supervisor Task Sidebar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Track detached tasks and model-spawned subagents in one supervisor registry and present them in an optional right sidebar without showing an empty task control.

**Architecture:** Existing task executors remain unchanged. `CodingAgent` emits explicit child lifecycle events, `HarnessSupervisor` normalizes both execution paths into `TaskRecord`s, and the desktop app renders active-project records through a reusable Makepad `TaskSidebar`.

**Tech Stack:** Rust, Tokio broadcast events, Makepad widgets/PortalList, existing Threadlane session discovery.

## Global Constraints

- Do not add dependencies or a new persistence format.
- Keep the existing synchronous `subagent` result contract and inline chat activity rail.
- Keep completed records only for the current application run.
- The right sidebar is 280 px wide and never opens automatically.
- Hide the header task control when the active project has no task records.
- Only detached `/task` records expose cancellation.
- Run `cargo check -p threadlane` and `git diff --check`.
- Visually verify through the Makepad Studio remote workflow.

---

### Task 1: Normalize task lifecycle in `HarnessSupervisor`

**Files:**
- Modify: `crates/threadlane-agent/src/events.rs`
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs`
- Modify: `crates/threadlane/src/state.rs`
- Test: `crates/threadlane-coding-agent/src/supervisor.rs`

**Interfaces:**
- Consumes: existing `AgentEvent`, `HarnessSupervisor::create_task`, and child tool-call IDs formatted as `subagent-{run_id}:{task_index}:{child_id}`.
- Produces: `AgentEvent::SubagentQueued`, `AgentEvent::SubagentStarted`, `AgentEvent::SubagentFinished`, `TaskKind`, expanded `TaskRecord`, and `HarnessSupervisor::observe_session_event(&self, project_id: &str, session_id: &str, session_file: Option<&Path>, event: &AgentEvent) -> bool`.

- [ ] **Step 1: Write failing supervisor reducer tests**

Add tests that construct a task map, apply child lifecycle/tool events, and assert
identity, ownership, activity, and terminal state:

```rust
#[test]
fn subagent_lifecycle_is_tracked_under_its_session() {
    let mut tasks = HashMap::new();
    let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");

    assert!(apply_subagent_event(
        &mut tasks,
        "project-1",
        "chat",
        Some(&session_file),
        None,
        &AgentEvent::SubagentQueued {
            run_id: 7,
            task_index: 1,
            agent: "reviewer".into(),
            task: "Review the patch".into(),
        },
    ));

    let task = tasks.get("subagent-7:1").unwrap();
    assert_eq!(task.kind, TaskKind::Subagent);
    assert_eq!(task.project_id, "project-1");
    assert_eq!(task.session_id, "chat");
    assert_eq!(task.agent, "reviewer");
    assert_eq!(task.summary, "Review the patch");
    assert_eq!(task.status, TaskStatus::Idle);
    assert!(!task.cancellable());

    assert!(apply_subagent_event(
        &mut tasks,
        "project-1",
        "chat",
        Some(&session_file),
        None,
        &AgentEvent::SubagentStarted {
            run_id: 7,
            task_index: 1,
        },
    ));
    assert_eq!(tasks["subagent-7:1"].status, TaskStatus::Running);
}

#[test]
fn child_tool_events_update_only_the_matching_subagent() {
    let mut tasks = HashMap::new();
    let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");
    for task_index in 0..2 {
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::SubagentQueued {
                run_id: 8,
                task_index,
                agent: format!("worker-{task_index}"),
                task: format!("Task {task_index}"),
            },
        );
    }

    apply_subagent_event(
        &mut tasks,
        "project-1",
        "chat",
        Some(&session_file),
        None,
        &AgentEvent::ToolExecutionStart {
            tool_call_id: "subagent-8:1:read-1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/lib.rs"}"#.into(),
        },
    );

    assert_eq!(tasks["subagent-8:0"].current_activity, None);
    assert_eq!(
        tasks["subagent-8:1"].current_activity.as_deref(),
        Some("read_file")
    );
}

#[test]
fn subagent_finish_records_success_and_failure() {
    let mut tasks = HashMap::new();
    let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");
    apply_subagent_event(
        &mut tasks,
        "project-1",
        "chat",
        Some(&session_file),
        Some("task-parent"),
        &AgentEvent::SubagentQueued {
            run_id: 9,
            task_index: 0,
            agent: "worker".into(),
            task: "Implement".into(),
        },
    );
    apply_subagent_event(
        &mut tasks,
        "project-1",
        "chat",
        Some(&session_file),
        Some("task-parent"),
        &AgentEvent::SubagentStarted {
            run_id: 9,
            task_index: 0,
        },
    );
    apply_subagent_event(
        &mut tasks,
        "project-1",
        "chat",
        Some(&session_file),
        Some("task-parent"),
        &AgentEvent::SubagentFinished {
            run_id: 9,
            task_index: 0,
            succeeded: false,
            error: Some("provider failed".into()),
        },
    );

    let task = &tasks["subagent-9:0"];
    assert_eq!(task.parent_task_id.as_deref(), Some("task-parent"));
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.current_activity.as_deref(), Some("provider failed"));
    assert!(task.finished_at_ms.is_some());
}

#[test]
fn background_task_tracks_current_tool_and_completion() {
    let mut task = TaskRecord {
        id: "task-1".into(),
        project_id: "project-1".into(),
        session_id: "task-1".into(),
        session_file: Some(PathBuf::from(
            "/repo/.threadlane/sessions/task-1.jsonl",
        )),
        parent_task_id: None,
        kind: TaskKind::Background,
        agent: "task".into(),
        summary: "Run checks".into(),
        current_activity: None,
        status: TaskStatus::Running,
        started_at_ms: 1,
        finished_at_ms: None,
    };

    assert!(apply_background_event(
        &mut task,
        &AgentEvent::ToolExecutionStart {
            tool_call_id: "command-1".into(),
            name: "run_command".into(),
            arguments: r#"{"command":"cargo check"}"#.into(),
        },
    ));
    assert_eq!(task.current_activity.as_deref(), Some("run_command"));

    assert!(apply_background_event(
        &mut task,
        &AgentEvent::AgentEnd {
            usage: Default::default(),
        },
    ));
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(task.finished_at_ms.is_some());
}

#[test]
fn unfinished_session_subagents_are_cancelled_when_the_parent_run_ends() {
    let supervisor = HarnessSupervisor::new(tempfile::tempdir().unwrap().path().to_path_buf());
    supervisor.observe_session_event(
        "project-1",
        "chat",
        None,
        &AgentEvent::SubagentQueued {
            run_id: 10,
            task_index: 0,
            agent: "worker".into(),
            task: "Inspect".into(),
        },
    );

    assert!(supervisor.finish_session_tasks("project-1", "chat"));
    let task = supervisor
        .list_tasks_for_project("project-1")
        .into_iter()
        .find(|task| task.id == "subagent-10:0")
        .unwrap();
    assert_eq!(task.status, TaskStatus::Cancelled);
    assert!(task.finished_at_ms.is_some());
}
```

- [ ] **Step 2: Run the focused tests and confirm they fail**

Run:

```bash
cargo test -p threadlane-coding-agent supervisor::tests -- --nocapture
```

Expected: compilation fails because the lifecycle variants, `TaskKind`, expanded
fields, and reducer do not exist.

- [ ] **Step 3: Add explicit lifecycle variants**

Extend `AgentEvent` in `crates/threadlane-agent/src/events.rs`:

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
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
},
```

In exhaustive GUI matches, treat these as supervisor-only events after forwarding
them to the supervisor; they must not create duplicate chat messages.

- [ ] **Step 4: Expand the supervisor record and add the reducer**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    Background,
    Subagent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub project_id: String,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub parent_task_id: Option<String>,
    pub kind: TaskKind,
    pub agent: String,
    pub summary: String,
    pub current_activity: Option<String>,
    pub status: TaskStatus,
    pub started_at_ms: u128,
    pub finished_at_ms: Option<u128>,
}

impl TaskRecord {
    pub fn cancellable(&self) -> bool {
        self.kind == TaskKind::Background
            && matches!(
                self.status,
                TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting
            )
    }

    pub fn active(&self) -> bool {
        matches!(
            self.status,
            TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting
        )
    }
}
```

Implement stable parsing and reduction:

```rust
fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn child_task_id(tool_call_id: &str) -> Option<String> {
    let tagged = tool_call_id.strip_prefix("subagent-")?;
    let mut parts = tagged.splitn(3, ':');
    let run_id = parts.next()?.parse::<u64>().ok()?;
    let task_index = parts.next()?.parse::<usize>().ok()?;
    parts.next()?;
    Some(format!("subagent-{run_id}:{task_index}"))
}

fn apply_subagent_event(
    tasks: &mut HashMap<String, TaskRecord>,
    project_id: &str,
    session_id: &str,
    session_file: Option<&Path>,
    parent_task_id: Option<&str>,
    event: &AgentEvent,
) -> bool {
    match event {
        AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent,
            task,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            tasks.insert(
                id.clone(),
                TaskRecord {
                    id,
                    project_id: project_id.to_owned(),
                    session_id: session_id.to_owned(),
                    session_file: session_file.map(Path::to_path_buf),
                    parent_task_id: parent_task_id.map(str::to_owned),
                    kind: TaskKind::Subagent,
                    agent: agent.clone(),
                    summary: task.clone(),
                    current_activity: None,
                    status: TaskStatus::Idle,
                    started_at_ms: now_ms(),
                    finished_at_ms: None,
                },
            );
            true
        }
        AgentEvent::SubagentStarted {
            run_id,
            task_index,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.status = TaskStatus::Running;
            true
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id, name, ..
        } => {
            let Some(id) = child_task_id(tool_call_id) else {
                return false;
            };
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.current_activity = Some(name.clone());
            true
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
            let Some(id) = child_task_id(tool_call_id) else {
                return false;
            };
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.current_activity = None;
            true
        }
        AgentEvent::SubagentFinished {
            run_id,
            task_index,
            succeeded,
            error,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.status = if *succeeded {
                TaskStatus::Completed
            } else {
                TaskStatus::Failed
            };
            task.current_activity = error.clone();
            task.finished_at_ms = Some(now_ms());
            true
        }
        _ => false,
    }
}
```

Add the public foreground observation entry point:

```rust
pub fn observe_session_event(
    &self,
    project_id: &str,
    session_id: &str,
    session_file: Option<&Path>,
    event: &AgentEvent,
) -> bool {
    apply_subagent_event(
        &mut self.tasks.lock().unwrap(),
        project_id,
        session_id,
        session_file,
        None,
        event,
    )
}
```

Add the parent-run cleanup entry point:

```rust
pub fn finish_session_tasks(&self, project_id: &str, session_id: &str) -> bool {
    let mut changed = false;
    let finished_at_ms = now_ms();
    for task in self.tasks.lock().unwrap().values_mut() {
        if task.project_id == project_id
            && task.session_id == session_id
            && task.kind == TaskKind::Subagent
            && task.active()
        {
            task.status = TaskStatus::Cancelled;
            task.current_activity = None;
            task.finished_at_ms = Some(finished_at_ms);
            changed = true;
        }
    }
    changed
}
```

Call the same reducer from the background event-forwarding task with
`Some(&tid)`, and initialize detached records with `TaskKind::Background`,
`agent: "task"`, `session_file: Some(final_session_file.clone())`, and
`session_id` from the session file stem.

When `submit_input` accepts the prompt, store it as the detached record's
`summary` before spawning the run:

```rust
if let Some(task) = self.tasks.lock().unwrap().get_mut(task_id) {
    task.summary = prompt.clone();
    task.status = TaskStatus::Running;
}
```

Use one reducer for the detached parent record:

```rust
fn apply_background_event(task: &mut TaskRecord, event: &AgentEvent) -> bool {
    match event {
        AgentEvent::AgentStart => {
            task.status = TaskStatus::Running;
            true
        }
        AgentEvent::AgentEnd { .. } => {
            task.status = TaskStatus::Completed;
            task.current_activity = None;
            task.finished_at_ms = Some(now_ms());
            true
        }
        AgentEvent::AgentError { error } => {
            task.status = TaskStatus::Failed;
            task.current_activity = Some(error.clone());
            task.finished_at_ms = Some(now_ms());
            true
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id, name, ..
        } if child_task_id(tool_call_id).is_none() => {
            task.current_activity = Some(name.clone());
            true
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. }
            if child_task_id(tool_call_id).is_none() =>
        {
            task.current_activity = None;
            true
        }
        _ => false,
    }
}
```

Call it from the existing background event loop before forwarding the event.
Set `finished_at_ms` in `cancel_task` as well, and mark every active subagent
whose `parent_task_id` matches the cancelled detached task as cancelled.

- [ ] **Step 5: Remove the obsolete GUI-only task reducer**

Delete `BackgroundTaskState` and `BackgroundTaskStatus` from
`crates/threadlane/src/state.rs`. Keep `GuiAgentEvent::BackgroundTask`; it is
still the wake-up/event-forwarding path from supervisor-owned detached agents.

- [ ] **Step 6: Run the focused tests**

Run:

```bash
cargo test -p threadlane-coding-agent supervisor::tests -- --nocapture
```

Expected: all supervisor tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/threadlane-agent/src/events.rs crates/threadlane-coding-agent/src/supervisor.rs crates/threadlane/src/state.rs
git commit -m "feat: unify supervisor task lifecycle"
```

---

### Task 2: Emit lifecycle events from model-spawned subagents

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs`

**Interfaces:**
- Consumes: `AgentEvent::SubagentQueued`, `AgentEvent::SubagentStarted`, `AgentEvent::SubagentFinished`, `SubagentRunContext::parent_event_tx`, and the existing process-global `run_id`.
- Produces: one queued, one start, and one terminal event per `AgentRunTask`, for parallel and sequential execution.

- [ ] **Step 1: Add a failing lifecycle test**

Extend the existing `model_subagent_tool_returns_awaited_child_output` test to
subscribe before executing the tool and collect lifecycle events afterward:

```rust
let mut event_rx = coding_agent.subscribe();
let results = coding_agent
    .execute_registered_tool_call(AgentToolCall::new(
        "subagent-call",
        "subagent",
        serde_json::json!({
            "parallel": true,
            "tasks": [
                {"agent": "scout", "task": "inspect"},
                {"agent": "reviewer", "task": "review"}
            ]
        })
        .to_string(),
    ))
    .await;

let events = std::iter::from_fn(|| event_rx.try_recv().ok()).collect::<Vec<_>>();
let queued = events
    .iter()
    .filter_map(|event| match event {
        AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent,
            task,
        } => Some((*run_id, *task_index, agent.clone(), task.clone())),
        _ => None,
    })
    .collect::<Vec<_>>();
let started = events
    .iter()
    .filter(|event| matches!(event, AgentEvent::SubagentStarted { .. }))
    .count();
let finished = events
    .iter()
    .filter(|event| matches!(event, AgentEvent::SubagentFinished { .. }))
    .count();

assert_eq!(queued.len(), 2);
assert_eq!(queued[0].0, queued[1].0);
assert_eq!(queued[0].1, 0);
assert_eq!(queued[1].1, 1);
assert_eq!(started, 2);
assert_eq!(finished, 2);
assert_eq!(results.len(), 1);
```

- [ ] **Step 2: Run the focused test and confirm failure**

Run:

```bash
cargo test -p threadlane-coding-agent model_subagent_tool_returns_awaited_child_output -- --nocapture
```

Expected: assertions fail because no lifecycle events are emitted.

- [ ] **Step 3: Wrap each worker execution with lifecycle emission**

Inside `run_subagents_with_context`, emit every queued event before constructing
the execution futures:

```rust
for (task_index, task) in tasks.iter().enumerate() {
    let _ = context
        .parent_event_tx
        .send(AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent: task.agent.clone(),
            task: task.task.clone(),
        });
}
```

Emit start after the worker acquires its execution permit and terminal state
after timeout/execution settles:

```rust
let event_tx = context.parent_event_tx.clone();
async move {
    let result = async {
        let _permit = context
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Subagent concurrency limiter closed".to_string())?;
        let _ = event_tx.send(AgentEvent::SubagentStarted {
            run_id,
            task_index,
        });
        timeout(
            SUBAGENT_TIMEOUT,
            run_subagent_task(config, task.task, context, run_id, task_index),
        )
        .await
        .map_err(|_| "Subagent timed out".to_string())?
    }
    .await;

    let (succeeded, error) = match &result {
        Ok(_) => (true, None),
        Err(error) => (false, Some(error.clone())),
    };
    let _ = event_tx.send(AgentEvent::SubagentFinished {
        run_id,
        task_index,
        succeeded,
        error,
    });
    result
}
```

Queued workers remain visible as `TaskStatus::Idle` until their execution permit
is acquired.

- [ ] **Step 4: Update exhaustive event tests**

Keep `subagent_ui_event` from re-emitting lifecycle events from a nested child by
adding them to its suppressed branch:

```rust
AgentEvent::SubagentQueued { .. }
| AgentEvent::SubagentStarted { .. }
| AgentEvent::SubagentFinished { .. } => None,
```

Add assertions beside `subagent_ui_events_do_not_override_parent_lifecycle`.

- [ ] **Step 5: Run coding-agent tests**

Run:

```bash
cargo test -p threadlane-coding-agent coding_agent::tests -- --nocapture
```

Expected: all coding-agent tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-coding-agent/src/coding_agent.rs
git commit -m "feat: report subagent lifecycle"
```

---

### Task 3: Build the reusable task sidebar

**Files:**
- Create: `crates/threadlane/src/components/task_sidebar.rs`
- Modify: `crates/threadlane/src/components/mod.rs`
- Modify: `crates/threadlane/src/panels/sessions/mod.rs`
- Modify: `crates/threadlane/src/panels/sessions/state.rs`
- Test: `crates/threadlane/src/components/task_sidebar.rs`
- Test: `crates/threadlane/src/panels/sessions/state.rs`

**Interfaces:**
- Consumes: `TaskRecord`, `TaskStatus`, and existing session discovery state.
- Produces: `TaskSidebar`, `TaskSidebarItem`, `TaskSidebarAction`, `task_sidebar_rows(&[TaskSidebarItem], Option<&str>)`, and `session_entry_for_file(&Path, &Path)`.

- [ ] **Step 1: Add failing pure row-model tests**

Define tests before the component implementation:

```rust
#[test]
fn sidebar_groups_current_session_first_and_active_tasks_first() {
    let items = vec![
        item("done", "chat-b", TaskStatus::Completed, 10),
        item("running", "chat-a", TaskStatus::Running, 20),
        item("older", "chat-a", TaskStatus::Completed, 5),
    ];

    let rows = task_sidebar_rows(&items, Some("chat-a"));
    assert!(matches!(
        &rows[0],
        TaskSidebarRow::SessionHeader { current: true, .. }
    ));
    assert!(matches!(
        &rows[1],
        TaskSidebarRow::Task(index) if items[*index].id == "running"
    ));
    assert!(matches!(
        &rows[2],
        TaskSidebarRow::Task(index) if items[*index].id == "older"
    ));
}

#[test]
fn header_is_hidden_only_when_the_project_has_no_tasks() {
    assert_eq!(task_header_state(&[]), (false, String::new()));
    assert_eq!(
        task_header_state(&[
            item("a", "chat-a", TaskStatus::Running, 1),
            item("b", "chat-a", TaskStatus::Completed, 2),
        ]),
        (true, "1".into())
    );
}
```

- [ ] **Step 2: Run the threadlane library tests and confirm failure**

Run:

```bash
cargo test -p threadlane task_sidebar -- --nocapture
```

Expected: compilation fails because the sidebar model does not exist.

- [ ] **Step 3: Implement the sidebar data model and typed actions**

Create:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSidebarItem {
    pub id: String,
    pub session_id: String,
    pub session_label: String,
    pub session_file: Option<PathBuf>,
    pub agent: String,
    pub summary: String,
    pub activity: String,
    pub status: TaskStatus,
    pub cancellable: bool,
    pub started_at_ms: u128,
}

#[derive(Clone, Debug, Default)]
pub enum TaskSidebarAction {
    Close,
    OpenSession {
        session_id: String,
        session_file: Option<PathBuf>,
    },
    Cancel(String),
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskSidebarRow {
    SessionHeader {
        session_id: String,
        label: String,
        current: bool,
    },
    Task(usize),
}

pub fn task_header_state(items: &[TaskSidebarItem]) -> (bool, String) {
    if items.is_empty() {
        return (false, String::new());
    }
    let active = items
        .iter()
        .filter(|item| {
            matches!(
                item.status,
                TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting
            )
        })
        .count();
    (true, active.to_string())
}
```

Implement `task_sidebar_rows` by grouping on `session_id`, ordering the current
session first, then ordering tasks by active state followed by descending
`started_at_ms`. Keep this function independent of Makepad so its behavior stays
unit-testable.

- [ ] **Step 4: Implement the Makepad widget**

Register `TaskSidebar` with:

- A 280 px `PanelSurface`.
- A 40 px header containing `"Tasks"` and an icon-only `close_btn`.
- A `PortalList` with `SessionHeader`, `TaskRow`, and `TaskRowCancellable`
  templates.
- Task row IDs: `task_surface`, `status_dot`, `summary_lbl`, `agent_lbl`,
  `activity_lbl`, and `cancel_btn`.
- Icon-only buttons must use `padding: 0`, `spacing: 0`, `text: ""`, and centered
  alignment.
- Running rows use `#x78aef0`, completed rows `#x67c58b`, failed rows
  `#xe86a64`, cancelled rows `#x8a94a3`, and idle/waiting rows `#xd2a85d`.

The widget stores:

```rust
#[rust]
items: Vec<TaskSidebarItem>,
#[rust]
rows: Vec<TaskSidebarRow>,
#[rust]
current_session_id: Option<String>,
```

Expose:

```rust
pub fn set_items(
    &mut self,
    cx: &mut Cx,
    items: Vec<TaskSidebarItem>,
    current_session_id: Option<String>,
) {
    if self.items == items && self.current_session_id == current_session_id {
        return;
    }
    self.rows = task_sidebar_rows(&items, current_session_id.as_deref());
    self.items = items;
    self.current_session_id = current_session_id;
    self.view.redraw(cx);
}
```

In `handle_event`, delegate to `self.view` first, then:

- Emit `Close` when `close_btn` is clicked.
- Iterate `PortalList::items_with_actions`.
- Emit `Cancel(item.id.clone())` for `cancel_btn`.
- Emit `OpenSession { session_id: item.session_id.clone(), session_file:
  item.session_file.clone() }` for a primary tap on the task surface.

Delegate `draw_walk` through the dereferenced view and configure each visible row
before `draw_all_unscoped`.

- [ ] **Step 5: Register the component**

Add `pub mod task_sidebar;` and call `task_sidebar::script_mod(vm)` after its
visual dependencies in `components/mod.rs`.

- [ ] **Step 6: Add session-file lookup**

Export:

```rust
pub fn session_entry_for_file(work_dir: &Path, session_file: &Path) -> Option<SessionEntry> {
    let work_dir = canonicalize_path(work_dir);
    let data = SESSIONS_DATA.read().unwrap();
    data.projects
        .iter()
        .find(|project| project.work_dir == work_dir)
        .and_then(|project| {
            project
                .sessions
                .iter()
                .find(|session| session.session_file == session_file)
        })
        .cloned()
}
```

Add a test that refreshes a temporary project containing one session and asserts
the exact file resolves.

- [ ] **Step 7: Run component and session tests**

Run:

```bash
cargo test -p threadlane task_sidebar -- --nocapture
cargo test -p threadlane session_entry_for_file -- --nocapture
```

Expected: all focused tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/threadlane/src/components/task_sidebar.rs crates/threadlane/src/components/mod.rs crates/threadlane/src/panels/sessions/mod.rs crates/threadlane/src/panels/sessions/state.rs
git commit -m "feat: add supervisor task sidebar"
```

---

### Task 4: Wire supervisor records into the workspace

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/state.rs`
- Test: `crates/threadlane/src/app/mod.rs`

**Interfaces:**
- Consumes: `HarnessSupervisor::observe_session_event`, `list_tasks_for_project`, `cancel_task`, `TaskSidebar`, `TaskSidebarAction`, `task_header_state`, and `session_entry_for_file`.
- Produces: active-project sidebar synchronization, header visibility/count, sidebar toggle, row navigation, and detached-task cancellation.

- [ ] **Step 1: Add a failing task-to-sidebar mapping test**

Extract a pure mapper and test that it preserves session ownership and
cancellability:

```rust
#[test]
fn supervisor_records_map_to_sidebar_items() {
    let record = TaskRecord {
        id: "task-1".into(),
        project_id: "project-1".into(),
        session_id: "chat".into(),
        session_file: Some(PathBuf::from("/repo/.threadlane/sessions/chat.jsonl")),
        parent_task_id: None,
        kind: TaskKind::Background,
        agent: "task".into(),
        summary: "Run checks".into(),
        current_activity: Some("run_command".into()),
        status: TaskStatus::Running,
        started_at_ms: 12,
        finished_at_ms: None,
    };

    let items = sidebar_items_from_records(vec![record], |_| Some("Checks".into()));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].session_label, "Checks");
    assert_eq!(items[0].activity, "run_command");
    assert!(items[0].cancellable);
}
```

- [ ] **Step 2: Run the focused test and confirm failure**

Run:

```bash
cargo test -p threadlane supervisor_records_map_to_sidebar_items -- --nocapture
```

Expected: compilation fails because the mapper does not exist.

- [ ] **Step 3: Add the sidebar to the existing content row**

Inside `content_row`, keep `chat_column` as `width: Fill` and add:

```text
task_sidebar := mod.components.TaskSidebar {
    width: 280
    height: Fill
    visible: false
}
```

Replace `tasks_btn` with a compact icon/count control named
`task_sidebar_btn`. It starts hidden and uses the existing subagent SVG. Keep
the count label bounded so values above 99 display as `"99+"`.

Add app state:

```rust
#[rust]
task_sidebar_open: bool,
```

Remove `background_tasks: BackgroundTaskState`.

Store the owning session file on each runtime so event handling never needs an
async agent lock:

```rust
struct SessionRuntime {
    agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    session_file: Option<PathBuf>,
    generation: Option<GenerationRun>,
    terminal_generation_id: Option<u64>,
    submitted_draft: Option<(u64, String)>,
    submitted_attachments: Option<(u64, Vec<ImageAttachment>)>,
    status: UiStatus,
    status_text: String,
    model: String,
    reasoning_effort: ReasoningEffort,
}

impl SessionRuntime {
    fn new(agent: CodingAgent, model: String, reasoning_effort: ReasoningEffort) -> Self {
        let session_file = agent.session_tree.file_path.clone();
        Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            session_file,
            generation: None,
            terminal_generation_id: None,
            submitted_draft: None,
            submitted_attachments: None,
            status: UiStatus::Ready,
            status_text: String::new(),
            model,
            reasoning_effort,
        }
    }
}
```

- [ ] **Step 4: Add one synchronization function**

Implement:

```rust
fn sync_task_sidebar(&mut self, cx: &mut Cx) {
    let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
        self.task_sidebar_open = false;
        self.ui
            .button(cx, ids!(task_sidebar_btn))
            .set_visible(cx, false);
        self.ui
            .widget(cx, ids!(task_sidebar))
            .set_visible(cx, false);
        return;
    };
    let Some(project_id) = self.supervisor_projects.get(&work_dir) else {
        return;
    };
    let records = self
        .supervisor
        .as_ref()
        .map(|supervisor| supervisor.list_tasks_for_project(project_id))
        .unwrap_or_default();
    let items = sidebar_items_from_records(records, |record| {
        record
            .session_file
            .as_deref()
            .and_then(|session_file| session_entry_for_file(&work_dir, session_file))
            .map(|entry| entry.title)
            .or_else(|| (record.session_id == "draft").then(|| "Project draft".to_owned()))
    });
    let (visible, count) = task_header_state(&items);

    let mut button = self.ui.button(cx, ids!(task_sidebar_btn));
    button.set_visible(cx, visible);
    button.set_text(cx, &if count == "0" { String::new() } else { count });

    if !visible {
        self.task_sidebar_open = false;
    }
    self.ui
        .widget(cx, ids!(task_sidebar))
        .set_visible(cx, visible && self.task_sidebar_open);

    if let Some(mut sidebar) = self
        .ui
        .widget(cx, ids!(task_sidebar))
        .borrow_mut::<TaskSidebar>()
    {
        sidebar.set_items(
            cx,
            items,
            self.workspace_state
                .active_key()
                .map(|key| key.session_id.clone()),
        );
    }
}
```

Canonicalize the work directory before looking up `supervisor_projects`, matching
the existing registration path.

- [ ] **Step 5: Feed foreground and background events into synchronization**

For `GuiAgentEvent::GenerationAgent`, before normal chat handling:

```rust
if let (Some(supervisor), Some(project_id)) = (
    self.supervisor.as_ref(),
    self.supervisor_projects.get(&work_dir),
) {
    let key = SessionKey::new(work_dir.clone(), session_id.clone());
    let session_file = self
        .session_runtimes
        .get(&key)
        .and_then(|runtime| runtime.session_file.as_deref());
    supervisor.observe_session_event(
        project_id,
        &session_id,
        session_file,
        &event,
    );
}
```

After foreground observation and after `GuiAgentEvent::BackgroundTask`, call
`sync_task_sidebar(cx)`. Remove the old `BackgroundTaskState::apply_agent_event`
and task-button text updates.

For `GuiAgentEvent::GenerationFinished`, call
`finish_session_tasks(project_id, &session_id)` before `sync_task_sidebar(cx)`.
This settles workers whose futures were dropped by a parent-generation abort;
normally completed workers are already terminal and remain unchanged.

Call `sync_task_sidebar` after:

- Startup project/session restoration.
- `activate_session`.
- Project draft selection.
- Project attach/detach.
- `/task` creation.

- [ ] **Step 6: Wire sidebar actions**

Handle the component's typed action:

```rust
let task_sidebar_uid = self.ui.widget(cx, ids!(task_sidebar)).widget_uid();
if let Some(action) = actions.find_widget_action(task_sidebar_uid) {
    match action.cast::<TaskSidebarAction>() {
        TaskSidebarAction::Close => {
            self.task_sidebar_open = false;
            self.sync_task_sidebar(cx);
        }
        TaskSidebarAction::OpenSession {
            session_id: _,
            session_file: None,
        } => {
            if let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) {
                self.select_project_draft(cx, work_dir);
            }
        }
        TaskSidebarAction::OpenSession {
            session_id: _,
            session_file: Some(session_file),
        } => {
            self.refresh_registered_sessions();
            if let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) {
                if let Some(entry) = session_entry_for_file(&work_dir, &session_file) {
                    self.activate_session(cx, entry);
                } else {
                    self.push_chat(
                        MsgRole::System,
                        format!("Task session is not available yet: {}", session_file.display()),
                    );
                }
            }
        }
        TaskSidebarAction::Cancel(task_id) => {
            if let Some(supervisor) = &self.supervisor {
                if let Err(error) = supervisor.cancel_task(&task_id) {
                    self.push_chat(MsgRole::System, error);
                }
            }
            self.sync_task_sidebar(cx);
        }
        TaskSidebarAction::None => {}
    }
}
```

Toggle `task_sidebar_open` when `task_sidebar_btn` is clicked. Delete the old
behavior that appended `background_tasks.summary()` to chat.

- [ ] **Step 7: Run focused tests and compile**

Run:

```bash
cargo test -p threadlane supervisor_records_map_to_sidebar_items -- --nocapture
cargo check -p threadlane
```

Expected: focused tests pass and the desktop crate compiles without warnings
introduced by this change.

- [ ] **Step 8: Commit**

```bash
git add crates/threadlane/src/app/mod.rs crates/threadlane/src/state.rs
git commit -m "feat: wire supervisor task sidebar"
```

---

### Task 5: Verify behavior and record the Makepad convention

**Files:**
- Modify: `AGENTS.md`
- Test: workspace commands and Makepad Studio runtime

**Interfaces:**
- Consumes: completed supervisor/sidebar implementation.
- Produces: validation evidence and one durable repository convention.

- [ ] **Step 1: Run narrow and workspace validation**

Run:

```bash
cargo test -p threadlane-coding-agent supervisor::tests -- --nocapture
cargo test -p threadlane-coding-agent coding_agent::tests -- --nocapture
cargo test -p threadlane task_sidebar -- --nocapture
cargo check -p threadlane
git diff --check
```

Expected: every command exits successfully.

- [ ] **Step 2: Inspect the final diff**

Run:

```bash
git status --short
git diff --stat
git diff -- crates/threadlane-agent/src/events.rs crates/threadlane-coding-agent/src/supervisor.rs crates/threadlane-coding-agent/src/coding_agent.rs crates/threadlane/src/components/task_sidebar.rs crates/threadlane/src/components/mod.rs crates/threadlane/src/panels/sessions/state.rs crates/threadlane/src/panels/sessions/mod.rs crates/threadlane/src/state.rs crates/threadlane/src/app/mod.rs AGENTS.md
```

Expected: only supervisor, lifecycle, sidebar, session lookup, app wiring,
documentation, and the approved design/plan files changed.

- [ ] **Step 3: Verify through Makepad Studio remote**

Use the repository's Makepad Studio flow to launch `threadlane` and observe:

1. With no tasks in the active project, no task control consumes header space.
2. `/task <prompt>` creates a visible row and shows the compact header control.
3. Clicking the control opens a 280 px right sidebar without obscuring chat.
4. A model-triggered parallel `subagent` call creates one row per worker.
5. Child tool activity updates the matching row.
6. Completed and failed rows settle without disappearing.
7. A detached running task shows a stop control; a model subagent does not.
8. Clicking a task row activates its owning session.
9. Closing the panel preserves tasks and leaves the compact control visible.
10. Switching projects shows only the selected project's records.

Capture a screenshot or Studio observation log as evidence. If the Studio bridge
is unavailable, report runtime verification as incomplete rather than claiming
the behavior was observed.

- [ ] **Step 4: Add the durable Makepad/task convention**

Add under the relevant `AGENTS.md` UI/state section:

```markdown
- Supervisor task presentation is a projection of `HarnessSupervisor` records.
  Detached `/task` agents and model-spawned subagents must emit normalized
  lifecycle events into that registry; do not add a second GUI-only task count
  or infer child ownership solely from transcript order.
- Optional header controls that summarize collections must be hidden when the
  collection is empty. Toggle `Button` visibility through a typed `.button(...)`
  reference and keep the corresponding panel hidden as well.
```

- [ ] **Step 5: Run final validation after documentation**

Run:

```bash
cargo check -p threadlane
git diff --check
```

Expected: both commands succeed.

- [ ] **Step 6: Commit**

```bash
git add AGENTS.md docs/superpowers/specs/2026-07-25-supervisor-task-sidebar-design.md docs/superpowers/plans/2026-07-25-supervisor-task-sidebar.md
git commit -m "docs: record supervisor task sidebar design"
```
