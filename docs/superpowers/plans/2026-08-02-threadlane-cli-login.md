# Threadlane CLI Provider Login Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a secure `/login` provider popup with Codex, Antigravity, and pasteable masked OpenAI API-key authentication.

**Architecture:** Reuse `threadlane-auth` for credential persistence and existing provider flows. Add a focused CLI login state module for provider selection and masked key entry, and keep runtime orchestration separate from rendering and input mapping. Login is modal: normal prompt submission is paused until the flow completes or is cancelled.

**Tech Stack:** Rust, Ratatui, Crossterm, Tokio, existing `threadlane-auth` OAuth/device flows.

## Global Constraints

- Never render or log the raw OpenAI key.
- Accept Crossterm paste events as a single masked-input operation.
- Preserve existing Codex and Antigravity credential formats and flows.
- Existing environment-variable credentials remain supported as fallback.
- Empty keys are rejected; cancellation and failed login preserve existing credentials.
- Run focused tests, `cargo check -p threadlane-cli`, and `git diff --check`.

---

### Task 1: Secure OpenAI key storage

**Files:**
- Modify: `crates/threadlane-auth/src/openai_auth.rs`
- Modify: `crates/threadlane-auth/src/lib.rs` only if exports are required
- Test: inline `openai_auth.rs` tests

**Interfaces:**
- Add `load_openai_api_key() -> Option<String>` and `save_openai_api_key(key: &str) -> Result<(), String>` using the existing `~/.threadlane` storage boundary.
- Keep `load_credentials()` behavior unchanged for Codex/device credentials.

- [ ] **Step 1: Write failing tests** for save/load round-trip, empty-key rejection, and serialization that never exposes the key in status text.
- [ ] **Step 2: Run focused auth tests and confirm failure.**
- [ ] **Step 3: Implement minimal storage with restrictive file permissions where supported and no secret logging.**
- [ ] **Step 4: Run `cargo test -p threadlane-auth` and confirm pass.**
- [ ] **Step 5: Commit with `feat: store openai cli api keys`**.

### Task 2: Add modal login state and provider flows

**Files:**
- Create: `crates/threadlane-cli/src/login.rs`
- Modify: `crates/threadlane-cli/src/main.rs`
- Modify: `crates/threadlane-cli/src/input.rs`
- Modify: `crates/threadlane-cli/src/runtime.rs`
- Modify: `crates/threadlane-cli/src/state.rs` only if shared status integration is required
- Test: `login.rs` and runtime/input unit tests

**Interfaces:**
- `LoginMode::{ProviderPicker, OpenAiKey}` and `LoginState` own only modal selection, masked key text, and safe status state.
- `/login` opens the provider picker; provider selection starts the existing Codex device-code or Antigravity PKCE flow asynchronously.
- OpenAI selection accepts `Event::Paste(String)` and typed characters, masks them, rejects empty Enter, saves non-empty keys, clears secret state, and emits safe status text.

- [ ] **Step 1: Write failing tests** for `/login` opening, provider selection, paste masking, empty-key rejection, Escape cancellation, and prompt blocking while login is active.
- [ ] **Step 2: Run focused CLI tests and confirm failure.**
- [ ] **Step 3: Implement the login state machine and input events without putting provider logic in the renderer.**
- [ ] **Step 4: Wire Codex and Antigravity flows through existing auth functions; do not duplicate OAuth/device protocol code.**
- [ ] **Step 5: Run `cargo test -p threadlane-cli` and relevant auth tests.**
- [ ] **Step 6: Commit with `feat: add cli provider login flow`**.

### Task 3: Render login popup and verify integration

**Files:**
- Modify: `crates/threadlane-cli/src/render.rs`
- Modify: `crates/threadlane-cli/src/commands.rs` for shared `/login` metadata
- Test: render/layout tests and full CLI/auth test suites

**Interfaces:**
- Render the provider popup and masked key-entry prompt using the existing completion-popup geometry and selection styling.
- Show only provider names, connection state, masked bullets, and bounded secret-free status/error messages.

- [ ] **Step 1: Write failing render tests** for provider popup visibility, masked key input, and safe status output.
- [ ] **Step 2: Implement the modal rendering without changing transcript/activity/plan layout.**
- [ ] **Step 3: Run `cargo test -p threadlane-cli`, `cargo test -p threadlane-auth`, and `cargo check -p threadlane-cli`.**
- [ ] **Step 4: Run `git diff --check` and `cargo run -p threadlane-cli -- --help`.**
- [ ] **Step 5: Manually verify `/login`, provider navigation, paste, Escape, and safe save status in a real terminal.**
- [ ] **Step 6: Commit with `feat: add cli login popup`**.
