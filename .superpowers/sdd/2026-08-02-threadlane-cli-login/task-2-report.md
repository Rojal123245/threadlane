# Task 2 Report — CLI Login Modal State and Provider Flows

Date: 2026-08-02

Implemented Task 2 for the Threadlane CLI login feature in the scoped CLI files.

Files changed:

- `crates/threadlane-cli/src/login.rs`
- `crates/threadlane-cli/src/main.rs`
- `crates/threadlane-cli/src/input.rs`
- `crates/threadlane-cli/src/runtime.rs`
- `crates/threadlane-cli/src/state.rs`
- `crates/threadlane-cli/src/commands.rs`

What changed:

- Added `login.rs` with:
  - `LoginMode::{ProviderPicker, OpenAiKey}`
  - `LoginProvider::{Codex, OpenAi, Antigravity}`
  - `LoginState` for modal-only state, masked key handling, safe status text, and pending async login attempts
  - async provider-flow helpers that reuse existing auth functions instead of duplicating protocol logic
- Added `/login` command metadata and parsing in the CLI command table.
- Added modal state to `AppState` and helpers to open/close login mode.
- Added paste-aware input support via `InputEvent::Paste(String)`.
- Routed login-active input through the login state machine:
  - provider picker navigation
  - OpenAI masked key entry
  - empty-key rejection
  - Escape cancellation
  - prompt blocking while login is active
- Reused existing auth functions:
  - Codex device code: `start_device_login`, `poll_device_token`
  - Antigravity PKCE: `generate_pkce_pair`, `build_authorization_url`, `listen_for_oauth_callback`, `exchange_code_for_tokens`
- Kept login output safe:
  - no raw OpenAI key stored in transcript messages
  - masked key state only
- Updated credential resolution so saved OpenAI keys can be reused by the CLI.

Focused tests added/updated:

- `login.rs`
  - masked typed/pasted OpenAI key input
  - cancellation clears secret state
- `input.rs`
  - paste event mapping
- `runtime.rs`
  - `/login` opens provider picker
  - provider picker selection flow
  - OpenAI empty-key rejection
  - paste masking
  - Escape cancellation
  - prompt blocking while login is active
  - paste still appends normally when login is closed
- `main.rs`
  - credential resolution falls back to saved OpenAI key

Verification run:

- `cargo test -p threadlane-cli` → passed (`44 passed`)
- `cargo test -p threadlane-auth` → passed (`13 passed`)
- `git diff --check` → clean

Notes:

- Renderer/UI presentation was intentionally left untouched for Task 3.
- The existing untracked plan file under `docs/superpowers/plans/2026-08-02-threadlane-cli-login.md` was left alone.

Review round 1 fix:

- Fixed the pending-login cancellation hole where Escape only closed the CLI login modal while the detached Codex device polling or Antigravity OAuth task kept running.
- The CLI runtime now owns the active login task abort handle, aborts it on pending-login Escape and on TUI shutdown, and ignores stale completion events by attempt ID / modal presence.
- Credential persistence was moved out of the background login task completion path:
  - Codex/OpenAI device polling now has a no-save variant used by the CLI login task.
  - Antigravity token exchange now has a no-save variant used by the CLI login task.
  - Credentials are only written after the runtime verifies the completion event still belongs to the active login flow.
- Added a regression test covering cancellation + stale completion ignoring without real network calls.

Additional verification for review round 1:

- `cargo test -p threadlane-cli` → passed (`45 passed`)
- `cargo test -p threadlane-auth` → passed (`13 passed`)

Review round 2 fix:

- Fixed the TUI shutdown ordering race where `run_tui` aborted the active login task before draining `login_rx`, which could drop a valid queued success event and skip credential persistence.
- The CLI now keeps the active login `JoinHandle`, drains queued login events before shutdown cancellation, then aborts only still-running flows, awaits task completion, and drains once more.
- This preserves valid active-attempt completion already queued at shutdown, still cancels genuinely pending flows, and continues to ignore stale events by attempt ID / modal state.
- Added a shutdown regression test covering queued login success during shutdown without real network calls.
- Unified CLI test HOME-mutation guards behind one crate-level lock so credential-path tests stop racing each other.

Additional verification for review round 2:

- `cargo test -p threadlane-cli` → passed (`46 passed`)
- `cargo test -p threadlane-auth` → passed (`13 passed`)
