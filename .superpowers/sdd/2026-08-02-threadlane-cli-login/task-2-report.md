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
