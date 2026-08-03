# Threadlane CLI Agentic MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the existing `threadlane-cli` crate into a terminal-first agentic harness with streaming responses, tool activity, cancellation, plans, model/reasoning commands, and reliable terminal cleanup.

**Architecture:** Keep `threadlane-cli` as a Ratatui/Crossterm presentation layer over the existing `CodingAgent` and `AgentEvent` stream. Reduce events into CLI-owned `AppState`; route commands back through existing provider, agent, and `SessionTree` APIs. Add shared runtime methods only where the current public API cannot support cancellation or session model updates.

**Tech Stack:** Rust, Tokio, Ratatui 0.29, Crossterm 0.28, existing Threadlane agent/provider/session crates.

## Global Constraints

- Keep the CLI in `crates/threadlane-cli/` as a separate workspace crate.
- Do not import Makepad or desktop `App` modules into the CLI.
- Do not create a second agent runtime, event bus, persistence format, or provider router.
- Reuse `CodingAgent::subscribe`, `CodingAgent::handle_input_with_images`, `AgentEvent`, `SessionPlan`, `ReasoningEffort`, and `SessionTree::set_model`; add only missing public `CodingAgent` seams.
- Preserve provider prefixes such as `antigravity/` when changing models.
- Preserve partial assistant output and mark active work cancelled on abort.
- Restore raw mode, alternate screen, cursor, and terminal styling on every exit path.
- Bound tool output and pasted input before rendering.
- Defer task sidebars, session switching UI, approvals, mouse support, rich markdown, autocomplete, and differential scrollback.
- Run focused tests before broader checks; finish with `cargo check`, relevant tests, and `git diff --check`.

---

### Task 1: Replace the placeholder CLI projection with tested agentic state

**Files:**
- Modify: `crates/threadlane-cli/src/ui.rs`
- Test: `crates/threadlane-cli/src/ui.rs`

**Interfaces:**
- Consumes: `threadlane_agent::events::AgentEvent`, `SessionPlan`, `PlanItem`, and existing `TranscriptMessage` display types.
- Produces: `AppState`, `ActivityItem`, `RunStatus`, and `reduce_agent_event(&mut AppState, AgentEvent)` used by `main.rs`.

- [ ] **Step 1: Write failing reducer tests**

Add tests for these exact transitions:

```rust
#[test]
fn message_updates_append_to_one_streaming_assistant() {
    let mut state = AppState::test_state();
    reduce_agent_event(&mut state, AgentEvent::MessageStart { role: "assistant".into() });
    reduce_agent_event(&mut state, AgentEvent::MessageUpdate {
        text_delta: Some("hel".into()),
        reasoning_delta: None,
        tool_call_name: None,
    });
    reduce_agent_event(&mut state, AgentEvent::MessageUpdate {
        text_delta: Some("lo".into()),
        reasoning_delta: None,
        tool_call_name: None,
    });
    assert_eq!(state.streaming_text(), "hello");
}

#[test]
fn tool_lifecycle_replaces_activity_status() {
    let mut state = AppState::test_state();
    reduce_agent_event(&mut state, AgentEvent::ToolExecutionStart {
        tool_call_id: "tool-1".into(),
        name: "read".into(),
        arguments: "{\"path\":\"src/main.rs\"}".into(),
    });
    assert_eq!(state.activities[0].status, ActivityStatus::Running);
    reduce_agent_event(&mut state, AgentEvent::ToolExecutionEnd {
        tool_call_id: "tool-1".into(),
        name: "read".into(),
        result: test_tool_result(),
    });
    assert_eq!(state.activities[0].status, ActivityStatus::Succeeded);
}
```

Also cover `AgentError`, `PlanUpdated`, `SubagentQueued`, `SubagentStarted`,
`SubagentFinished`, and cancellation status. Keep the test helpers local to
the module; do not add a fixture crate.

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p threadlane-cli ui::tests
```

Expected: FAIL because the new projection fields, reducer, and test helpers do
not yet exist.

- [ ] **Step 3: Add the minimal state model and reducer**

Replace the current placeholder fields with:

```rust
pub struct AppState {
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub work_dir: String,
    pub messages: Vec<TranscriptMessage>,
    pub composer: String,
    pub streaming: Option<StreamingMessage>,
    pub activities: Vec<ActivityItem>,
    pub plan: Option<SessionPlan>,
    pub status: RunStatus,
    pub scroll: u16,
}
```

Use `tool_call_id` as activity identity. Keep text deltas in the streaming
projection until `MessageEnd` commits final assistant text to the transcript.
Bound activity arguments and partial tool output with the existing standard
string helpers or a small local truncation function.

- [ ] **Step 4: Run the reducer tests**

Run `cargo test -p threadlane-cli ui::tests`; expect all state tests to pass.

- [ ] **Step 5: Commit the state slice**

```bash
git add crates/threadlane-cli/src/ui.rs
git commit -m "feat: add cli agent event state"
```

---

### Task 2: Render transcript, streaming output, activity, and plan

**Files:**
- Create: `crates/threadlane-cli/src/state.rs`
- Create: `crates/threadlane-cli/src/render.rs`
- Modify: `crates/threadlane-cli/src/ui.rs` as a compatibility facade
- Test: `crates/threadlane-cli/src/render.rs`

**Interfaces:**
- Consumes: `AppState` from Task 1.
- Produces: `pub fn render(frame: &mut Frame, state: &AppState)` with stable header, transcript, activity, plan, composer, and footer regions.

- [ ] **Step 1: Add layout-focused tests**

Test pure helpers rather than invoking a terminal:

```rust
#[test]
fn empty_activity_and_plan_do_not_create_empty_sections() {
    let state = AppState::test_state();
    let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
    assert_eq!(sections.activity.height, 0);
    assert_eq!(sections.plan.height, 0);
}

#[test]
fn active_plan_and_activity_get_bounded_height() {
    let mut state = AppState::test_state_with_plan(20);
    state.activities = test_activities(20);
    let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
    assert!(sections.transcript.height >= 1);
    assert!(sections.plan.height < 30);
    assert!(sections.activity.height < 30);
}
```

- [ ] **Step 2: Run the layout tests and verify failure**

Run `cargo test -p threadlane-cli ui::tests`; expect failure because the
section layout helper does not exist.

- [ ] **Step 3: Split state and rendering into focused modules**

Move `AppState`, `ActivityItem`, `RunStatus`, `StreamingMessage`, and
`reduce_agent_event` from `ui.rs` into `state.rs`. Move the existing render
functions and the new layout helpers into `render.rs`. Keep `ui.rs` limited to
module declarations and re-exports:

```rust
mod render;
mod state;

pub use render::render;
pub use state::{reduce_agent_event, ActivityItem, AppState, RunStatus, StreamingMessage};
```

Keep `ui::render` and `ui::AppState` valid for `main.rs` while making future
command, input, and overlay modules independent of the renderer.

- [ ] **Step 4: Implement the compact Ratatui layout**

Keep the existing `render_header`, `render_transcript`, `render_input`, and
`render_footer` structure, but insert conditional activity and plan regions:

```text
header
transcript + streaming tail
activity (only when non-empty)
plan (only when non-empty)
composer
footer
```

Render activity status with text as well as color (`running`, `done`, `failed`,
`cancelled`). Render plan item status using existing plan item data, not a new
plan type. Keep the transcript viewport pinned to the bottom while generating;
manual scroll disables pinning until the user returns to the end.

- [ ] **Step 5: Run tests and inspect the generated frame**

Run `cargo test -p threadlane-cli ui::tests` and `cargo check -p threadlane-cli`.
Use the existing Ratatui test backend if available; otherwise keep the tests on
pure layout/text helpers and manually inspect the CLI in Task 6.

- [ ] **Step 6: Commit the renderer slice**

```bash
git add crates/threadlane-cli/src/ui.rs
git commit -m "feat: render cli agent activity and plans"
```

---

### Task 3: Make terminal initialization and event polling exception-safe

**Files:**
- Modify: `crates/threadlane-cli/src/tui.rs`
- Create: `crates/threadlane-cli/src/input.rs`
- Modify: `crates/threadlane-cli/src/main.rs`
- Test: `crates/threadlane-cli/src/tui.rs`
- Test: `crates/threadlane-cli/src/input.rs`

**Interfaces:**
- Consumes: existing `tui::init`, `tui::restore`, Crossterm events, and Ratatui terminal type.
- Produces: an idempotent `TerminalGuard`/restore path and an input event adapter used by `run_tui`.

- [ ] **Step 1: Write lifecycle tests**

Test that cleanup is idempotent and that a cleanup guard remains usable after a
normal restore call:

```rust
#[test]
fn terminal_cleanup_is_idempotent() {
    let mut cleanup = CleanupState::new_for_test();
    cleanup.restore().unwrap();
    cleanup.restore().unwrap();
    assert!(cleanup.is_restored());
}
```

- [ ] **Step 2: Run the test and verify failure**

Run `cargo test -p threadlane-cli tui::tests`; expect failure because cleanup
state is not represented independently from the current free function.

- [ ] **Step 3: Implement guarded setup and restoration**

Make setup enable raw mode, alternate screen, hide cursor, and panic cleanup
only after all setup calls succeed. Make the guard restore in `Drop`, while
retaining an explicit `restore()` for normal shutdown. Ignore only the
already-restored state; return real Crossterm errors to the caller.

Keep terminal setup/restore in `tui.rs`. Add `input.rs` with a small CLI input
enum containing `Submit`, `CancelOrQuit`, `Backspace`, `Character(char)`,
`ScrollUp`, `ScrollDown`, and `Resize`, plus a pure Crossterm-key mapping
function. `main.rs` consumes the adapter but does not interpret raw key codes.

- [ ] **Step 4: Run lifecycle tests and compile**

Run `cargo test -p threadlane-cli tui::tests` and
`cargo check -p threadlane-cli`.

- [ ] **Step 5: Commit terminal safety**

```bash
git add crates/threadlane-cli/src/tui.rs crates/threadlane-cli/src/main.rs
git commit -m "fix: restore cli terminal state safely"
```

---

### Task 4: Wire the shared agent event loop and cancellation

**Files:**
- Create: `crates/threadlane-cli/src/runtime.rs`
- Modify: `crates/threadlane-cli/src/main.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs` only if no public cancellation method can be reused
- Test: `crates/threadlane-cli/src/main.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs` only if the public cancellation seam changes

**Interfaces:**
- Consumes: `CodingAgent::subscribe`, `CodingAgent::handle_input_with_images`, `AgentEvent`, and the existing internal cancellation path.
- Produces: `runtime::run_tui(work_dir, model)`, `spawn_prompt`, and a CLI-safe cancellation call that leaves partial output in `AppState`.

- [ ] **Step 1: Add event-loop tests around pure input dispatch**

Test these transitions without starting a terminal:

```rust
#[test]
fn enter_submits_only_when_idle_and_composer_is_nonempty() {
    let mut state = AppState::test_state();
    assert_eq!(dispatch_input(&mut state, InputEvent::Submit), Action::Submit("".into()));
    state.composer = "inspect the project".into();
    assert_eq!(dispatch_input(&mut state, InputEvent::Submit), Action::Submit("inspect the project".into()));
}

#[test]
fn escape_cancels_generation_before_quitting() {
    let mut state = AppState::test_state_generating();
    assert_eq!(dispatch_input(&mut state, InputEvent::CancelOrQuit), Action::Cancel);
    state.status = RunStatus::Idle;
    assert_eq!(dispatch_input(&mut state, InputEvent::CancelOrQuit), Action::Quit);
}
```

- [ ] **Step 2: Run the tests and verify failure**

Run `cargo test -p threadlane-cli main::tests`; expect failure because the
input adapter and action type do not exist.

- [ ] **Step 3: Move the interactive loop into a runtime module**

Move `run_tui` and its event-loop helpers from `main.rs` into `runtime.rs`.
Leave `main.rs` responsible only for argument parsing, work-directory
resolution, headless dispatch, and calling `runtime::run_tui`. Keep event
reduction in `state.rs`, rendering in `render.rs`, terminal ownership in
`tui.rs`, and key mapping in `input.rs`.

- [ ] **Step 4: Replace the polling loop with shared event reduction**

In `run_tui`:

1. initialize the guarded terminal;
2. construct `CodingAgent` using the existing `CodingAgentOptions` path;
3. subscribe with `CodingAgent::subscribe()`;
4. draw the current `AppState`;
5. poll Crossterm input with a short timeout;
6. dispatch input actions;
7. drain available `AgentEvent`s and call `reduce_agent_event`;
8. redraw after state changes;
9. on quit, cancel active work, restore the terminal, and return.

Do not append an empty assistant transcript entry before the first event. Let
`MessageStart`/`MessageUpdate` create the streaming projection and let
`MessageEnd` commit it.

- [ ] **Step 5: Expose only the missing cancellation seam**

First search for a public cancellation method. If none exists, add this small
method to `CodingAgent`:

```rust
pub fn cancel(&mut self) -> Result<(), String> {
    self.begin_cancellation().map(|_| ())
}
```

If the existing guard must remain alive for cancellation to complete, return
the guard from a CLI-owned field instead of discarding it, and add a focused
test proving a running operation produces terminal cancellation events. Do not
reimplement tool cancellation in the CLI.

- [ ] **Step 6: Verify event wiring**

Run:

```bash
cargo test -p threadlane-cli
cargo test -p threadlane-coding-agent coding_agent
cargo check -p threadlane-cli
```

- [ ] **Step 7: Commit the runtime wiring**

```bash
git add crates/threadlane-cli/src/main.rs crates/threadlane-coding-agent/src/coding_agent.rs
git commit -m "feat: connect cli to agent event stream"
```

---

### Task 5: Add slash commands for model, reasoning, plan, and session controls

**Files:**
- Create: `crates/threadlane-cli/src/commands.rs`
- Modify: `crates/threadlane-cli/src/main.rs`
- Modify: `crates/threadlane-cli/src/ui.rs`
- Test: `crates/threadlane-cli/src/commands.rs`

**Interfaces:**
- Consumes: `ReasoningEffort::{from_label,label}`, provider model validation, `CodingAgent::set_reasoning_effort`, and the active session tree’s `set_model` path.
- Produces: `parse_command(&str) -> Result<Command, CommandError>` and `execute_command(&mut CommandContext, Command) -> CommandResult`.

- [ ] **Step 1: Write parser tests**

Cover the exact command forms:

```rust
#[test]
fn parses_model_and_reasoning_commands() {
    assert_eq!(parse_command("/model").unwrap(), Command::ShowModel);
    assert_eq!(parse_command("/model antigravity/gemini").unwrap(), Command::SetModel("antigravity/gemini".into()));
    assert_eq!(parse_command("/reasoning high").unwrap(), Command::SetReasoning(ReasoningEffort::High));
    assert_eq!(parse_command("/help").unwrap(), Command::Help);
}

#[test]
fn rejects_unknown_commands_and_extra_model_arguments() {
    assert!(matches!(parse_command("/wat"), Err(CommandError::Unknown(_))));
    assert!(parse_command("/model a b").is_err());
}
```

- [ ] **Step 2: Run parser tests and verify failure**

Run `cargo test -p threadlane-cli commands::tests`; expect failure because the
command module does not exist.

- [ ] **Step 3: Implement the command parser**

Support:

```text
/model
/model <provider/model>
/models
/reasoning
/reasoning <off|minimal|low|medium|high|xhigh>
/plan
/clear
/session
/help
/quit
```

Reject empty model names, unknown reasoning labels, and trailing arguments.
Keep command output as `CommandResult::Message(String)` or
`CommandResult::Quit` so the UI can display it without a second output path.

- [ ] **Step 4: Execute commands through existing APIs**

Create a `CommandContext` containing mutable references to the shared agent,
active session metadata, and `AppState`. Reject mutating commands while
`AppState.status` is running. The current code exposes
`SessionTree::set_model` but no public `CodingAgent::set_model`; add
`pub fn set_model(&mut self, model: String) -> Result<(), String>` only if
needed, delegating to the agent/provider model configuration without adding a
second router. For `/model <value>`, validate the selected provider/model
through existing provider support, update the agent configuration, and call
`SessionTree::set_model` for persistence. Preserve the full provider prefix.
For `/reasoning <level>`, call
`CodingAgent::set_reasoning_effort` and update the header projection. `/plan`
prints the current plan or a concise empty-state message; `/clear` clears only
visible transcript projection; `/session` prints current work directory,
model, and reasoning level; `/help` prints the command list.

- [ ] **Step 5: Add command dispatch to the composer path**

When submitted input starts with `/`, parse and execute it instead of calling
`handle_input_with_images`. Append command results as system/status messages.
Ordinary input continues through the existing agent method.

- [ ] **Step 6: Run command and integration tests**

Run `cargo test -p threadlane-cli commands::tests` and
`cargo check -p threadlane-cli`.

- [ ] **Step 7: Commit command support**

```bash
git add crates/threadlane-cli/src/commands.rs crates/threadlane-cli/src/main.rs crates/threadlane-cli/src/ui.rs
git commit -m "feat: add cli model and reasoning commands"
```

---

### Task 6: Validate the complete CLI MVP

**Files:**
- Modify: `crates/threadlane-cli/src/main.rs` only for narrowly scoped validation fixes
- Test: existing CLI and coding-agent test modules

- [ ] **Step 1: Run focused automated checks**

```bash
cargo test -p threadlane-cli
cargo test -p threadlane-coding-agent coding_agent
cargo check -p threadlane-cli
git diff --check
```

- [ ] **Step 2: Run the CLI manually in a PTY**

Verify all of the following:

1. startup enters the alternate screen and hides the cursor;
2. Enter submits a prompt and assistant text streams incrementally;
3. tool start/end activity appears and settles visibly;
4. the plan region updates from `PlanUpdated` events;
5. Escape cancels active work and preserves partial output;
6. a second Escape while idle exits;
7. Ctrl+C exits and restores the original terminal state;
8. terminal resize redraws without panic or stale layout;
9. `/model`, `/models`, `/reasoning`, `/plan`, `/session`, `/help`, and `/clear` work while idle;
10. model changes preserve `antigravity/` and other provider prefixes;
11. model/reasoning changes are rejected while generation is active;
12. provider and tool errors remain visible in the transcript/activity area.

- [ ] **Step 3: Add a regression test for every discovered defect**

If manual verification finds a defect, add the smallest reducer, parser, or
terminal-state test that fails before the fix and passes after it. Keep runtime
visual issues out of shared agent code unless the event contract is actually
wrong.

- [ ] **Step 4: Run final checks**

```bash
cargo test -p threadlane-cli
cargo test -p threadlane-coding-agent
cargo check -p threadlane-cli
git diff --check
```

- [ ] **Step 5: Commit final validation fixes**

```bash
git add crates/threadlane-cli crates/threadlane-coding-agent
git commit -m "test: verify cli agentic mvp"
```

## Completion criteria

- The CLI shares the existing agent and persistence runtime.
- Streaming assistant text, tool activity, plans, cancellation, and errors are visible.
- Model and reasoning commands work while idle and persist model selection.
- Active generation cannot be accidentally replaced by a model change.
- Terminal state is restored on normal exit, cancellation, error, and panic.
- Focused tests, CLI checks, and manual PTY verification pass.
