# Threadlane CLI Agentic MVP Design

## Goal

Make `crates/threadlane-cli` a useful terminal-first agentic harness while
sharing Threadlane's existing agent, provider, session, plan, and event
implementations with the Makepad desktop application.

## Scope

The MVP includes:

- interactive prompt input;
- streaming assistant output;
- visible tool activity;
- generation cancellation;
- session plan display;
- model and reasoning-effort commands;
- safe terminal setup and restoration.

The MVP does not include task sidebars, session-switching UI, approvals, mouse
support, rich markdown, or OMP-style differential scrollback.

## Architecture

`threadlane-cli` is a separate workspace crate and presentation layer. It does
not create a second agent runtime or persistence format. It uses Ratatui and
Crossterm for terminal rendering and input, while the existing
`threadlane-agent`, `threadlane-coding-agent`, provider, and session crates
remain the source of truth for execution and persistence.

```text
stdin / key events
       │
       ▼
CLI actions ───────────────┐
                           ▼
                    shared CodingAgent
                           │
                    AgentEvent stream
                           ▼
                    CLI state reducer
                           │
                           ▼
                    Ratatui render frame
```

The CLI owns a presentation projection, not a replacement for desktop state:

```rust
struct AppState {
    transcript: Vec<TranscriptItem>,
    composer: String,
    streaming: Option<StreamingMessage>,
    activities: Vec<ActivityItem>,
    plan: Option<SessionPlan>,
    status: RunStatus,
    scroll: usize,
}
```

## Event model

The CLI subscribes to the existing `AgentEvent` stream and reduces events into
`AppState`.

- message start/update events append or update streaming assistant text;
- tool call/execution start events create activity rows with bounded summaries;
- tool execution/call end events settle activity rows as success, failure, or
  cancellation;
- agent start/end/error events update run status;
- plan events replace the current session-plan projection;
- subagent lifecycle events become compact delegated-work rows;
- abort events preserve partial assistant output and mark active work cancelled.

Every state change schedules a redraw. Rendering never executes agent work.

## Layout and interaction

```text
┌─ project / model / status ─────────────────────────────┐
│ transcript + streaming response                         │
│                                                        │
│  activity: read file …                                  │
│  activity: edit file …                                  │
│                                                        │
├─ plan --------------------------------------------------┤
│ [x] inspect runtime                                     │
│ [ ] wire CLI events                                     │
├─ composer ----------------------------------------------┤
│ > user prompt                                           │
└─ keys: Enter send · Esc cancel · Ctrl+C quit            ┘
```

- Enter submits the composer;
- Escape cancels active generation, and a second Escape exits only while idle;
- Up/Down navigate prompt history or the focused activity surface;
- Ctrl+C restores the terminal before exit;
- the composer is editable while idle;
- steering/follow-up queues remain deferred until the shared runtime exposes
  them without duplicating queue semantics;
- empty plans and activities consume no unnecessary space;
- raw tool payloads are bounded and secondary to the transcript.

## Commands

Slash commands are parsed locally by `threadlane-cli`:

```text
/model                         show current model
/model <provider/model>        switch model
/models                        list available models
/reasoning                     show reasoning effort
/reasoning <level>             switch reasoning effort
/plan                          show or toggle the current plan
/clear                         clear the visible transcript
/session                       show current session metadata
/help                          show commands
/quit                          exit cleanly
```

Command execution uses existing model validation, provider-prefix routing,
`ReasoningEffort`, and session metadata APIs. Model changes are rejected while
generation is active; idle changes update both runtime and persisted session
selection. Autocomplete is deferred until the parser is stable. `/task` and
expanded subagent controls are deferred until foreground generation works.

## Terminal safety

- enter raw mode and alternate-screen mode only after initialization succeeds;
- restore raw mode, cursor visibility, alternate screen, and terminal colors on
  every exit path;
- install panic/unwind cleanup before entering the event loop;
- cancel active generation on the first Ctrl+C and exit only while idle;
- preserve partial assistant output on cancellation;
- show provider/tool failures as visible status or activity rows;
- redraw from current state after terminal resize;
- bound rendered tool output and pasted input.

## Implementation boundaries

Expected MVP work stays in the existing CLI crate plus only the shared event or
runtime seams required to expose already-supported behavior:

- `crates/threadlane-cli/src/main.rs` — CLI entry, shared runtime creation,
  event loop, command dispatch, and shutdown;
- `crates/threadlane-cli/src/tui.rs` — terminal initialization, restoration,
  resize, and event polling;
- `crates/threadlane-cli/src/ui.rs` — `AppState`, reducers, layout, and
  Ratatui widgets;
- `crates/threadlane-cli/Cargo.toml` — only existing workspace dependencies;
- shared agent files only when the current public event/API surface cannot
  represent the required streaming, cancellation, plan, or model behavior.

No desktop Makepad module is imported by the CLI.

## Verification

- reducer tests cover streaming text, tool lifecycle, failure, cancellation,
  plans, and subagents;
- command tests cover valid commands, invalid arguments, and model changes
  during active generation;
- state tests prove partial output survives cancellation;
- terminal lifecycle smoke tests use a mock backend where practical;
- run `cargo check -p threadlane-cli`;
- run `cargo test -p threadlane-cli`;
- run focused `cargo test -p threadlane-coding-agent` tests for touched runtime
  behavior;
- run `git diff --check`;
- manually verify PTY cleanup, resize, Ctrl+C, streaming, model switching, and
  reasoning changes.

## OMP lessons applied

- retain the separation between runtime/session orchestration and TUI
  components;
- treat the transcript as durable history and the streaming tail as mutable
  presentation state;
- centralize focus, overlays, input dispatch, and redraw scheduling in the TUI
  runtime;
- normalize tool and subagent lifecycle into compact activity records;
- add renderer sophistication only after the agentic event contract proves
  useful.
