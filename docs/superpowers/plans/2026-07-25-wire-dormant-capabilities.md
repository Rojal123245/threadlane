# Wire Dormant Capabilities Implementation Plan

> **Superseded extension design:** Full-trust/native extension executables and
> their approval/revocation UI described below are historical and unsupported.
> Threadlane extensions are WASI modules, and LSP launches language servers
> through brokered process capability.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the tested supervisor, package, trust, and extension-management APIs reachable through Threadlane’s desktop application instead of leaving them dormant.

**Architecture:** Keep the current chat/session runtime unchanged. Add `HarnessSupervisor` only for explicit background tasks, with app-owned task state and typed events; the Makepad app remains the only UI/event-thread owner. Surface package/trust operations and background tasks in the existing Projects sidebar/context-menu pattern and synchronize background work via channels and `SignalToUI`.

**Tech Stack:** Rust, Makepad, Tokio, existing `threadlane-coding-agent` capability/supervisor/WASI APIs.

## Global Constraints

- Do not add dependencies or blanket lint allowances.
- Preserve the existing session and project persistence format unless a migration is explicitly required.
- Keep full-trust extension execution denied until explicit user approval is persisted.
- Background work sends typed events; UI mutations happen on the Makepad event thread.
- Validate with `cargo check -p threadlane`, focused tests, `cargo clippy --workspace --all-targets --all-features`, and `just hawkcheck`.

---

### Task 1: Promote the capability-management API

**Files:**
- Modify: `crates/threadlane-coding-agent/src/packages.rs`
- Modify: `crates/threadlane-coding-agent/src/full_trust_extension.rs`
- Modify: `crates/threadlane-coding-agent/src/capabilities.rs`
- Modify: `crates/threadlane-coding-agent/src/lib.rs`
- Test: `crates/threadlane-coding-agent/tests/supervisor_tests.rs`

**Interfaces:**
- Produces `PackageManager::install_from_local`, `PackageManager::remove_package`, and `TrustStore::revoke` as supported crate APIs.
- Produces capability catalog entries containing package scope, enabled state, and trust/revision status for UI presentation.

- [x] **Step 1: Add failing integration tests**

Extend `supervisor_tests.rs` to install a temp package containing `threadlane-package.json`, assert it appears in `CapabilityCatalog::discover`, remove it, and assert it no longer appears. Add a trust-revocation assertion after `TrustStore::approve`.

- [x] **Step 2: Run the focused test**

Run: `cargo test -p threadlane-coding-agent --test supervisor_tests`

Expected: FAIL because installation/removal and revocation are not public supported operations.

- [x] **Step 3: Promote and use the existing operations**

Expose the existing `PackageManager` and `TrustStore` methods through `lib.rs`; do not duplicate filesystem or trust logic. Add any required read-only metadata accessors to `CapabilityCatalog`.

- [x] **Step 4: Re-run the focused test**

Run: `cargo test -p threadlane-coding-agent --test supervisor_tests`

Expected: PASS.

### Task 2: Make supervisor tasks an app runtime service

**Files:**
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs`
- Modify: `crates/threadlane/src/state.rs`
- Modify: `crates/threadlane/src/app/mod.rs`
- Test: `crates/threadlane-coding-agent/tests/supervisor_tests.rs`

**Interfaces:**
- Consumes `HarnessSupervisor::{register_project, create_task, submit_input, cancel_task, subscribe}`.
- Produces app-owned background-task state keyed by project work directory and typed events for status/output updates; it does not own chat sessions or transcripts.

- [x] **Step 1: Add a failing supervisor-event test**

Assert that submitting input changes a task from `Idle` to `Running` and forwards agent events through `HarnessSupervisor::subscribe`.

- [x] **Step 2: Run the focused test**

Run: `cargo test -p threadlane-coding-agent --test supervisor_tests`

Expected: FAIL if task status/event delivery is not observable.

- [x] **Step 3: Add typed app integration**

Store one `HarnessSupervisor` in app state, register attached projects, and create background tasks only through an explicit task action. Forward supervisor events onto a dedicated app task-state channel; do not create/select a supervisor task for ordinary chat sessions or merge its transcript into chat history.

- [x] **Step 4: Re-run focused tests and compile**

Run: `cargo test -p threadlane-coding-agent --test supervisor_tests && cargo check -p threadlane`

Expected: PASS.

### Task 3: Add minimal package and trust controls

**Files:**
- Modify: `crates/threadlane/src/panels/sessions/*`
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/components/*` only if an existing control cannot be reused

**Interfaces:**
- Consumes the capability catalog and package/trust operations from Task 1.
- Produces typed actions: install package, remove package, approve revision, revoke approval, refresh capabilities.

- [x] **Step 1: Add a failing state-level test**

Add a test for the app capability-state reducer: a refresh populates package/extension rows, and a revoke action marks the matching full-trust extension disabled.

- [x] **Step 2: Run the focused test**

Run: `cargo test -p threadlane capability`

Expected: FAIL because app capability state/actions do not exist.

- [x] **Step 3: Implement the smallest UI**

Add a sidebar capability section using existing list/context-menu patterns. Show package name, scope, and trust status; provide install/remove and approve/revoke actions. Require an explicit confirmation before approving or revoking full-trust execution.

- [x] **Step 4: Verify the focused test**

Run: `cargo test -p threadlane capability`

Expected: PASS.

### Task 4: Remove legacy WASI convenience methods

**Files:**
- Modify: `crates/threadlane-coding-agent/src/wasi_extension.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Retains the existing `*_with_effects` and broker-request paths.
- Produces one canonical tool/command/hook invocation path with no unused wrapper APIs.

- [x] **Step 1: Confirm the canonical paths are covered**

Review `wasi_tests.rs` coverage for `execute_tool_with_broker_requests`, `execute_command_with_effects`, and `execute_hook_with_effects`. Add coverage only for an uncovered retained path.

- [x] **Step 2: Run the focused test**

Run: `cargo test -p threadlane-coding-agent --test wasi_tests`

Expected: PASS, proving the retained paths preserve broker requests/results.

- [x] **Step 3: Delete the unused convenience wrappers**

Delete `WasiExtension::{call_tool, call_command, call_hook, call}` and the unused `WasiExtensionManager` convenience methods that discard effects. Do not change the existing effect-preserving methods or their broker dispatch behavior.

- [x] **Step 4: Re-run the focused test**

Run: `cargo test -p threadlane-coding-agent --test wasi_tests`

Expected: PASS with no unused wrapper warnings.

### Task 5: Final warning and behavior audit

**Files:**
- Modify: affected files only
- Test: workspace checks

- [x] **Step 1: Run the full validation set**

Run: `cargo test -p threadlane-coding-agent --test supervisor_tests && cargo test -p threadlane-coding-agent --test wasi_tests && cargo check -p threadlane && cargo clippy --workspace --all-targets --all-features && just hawkcheck && git diff --check`

Expected: all commands exit 0; no workspace-owned unused/dead-code warnings remain.

- [ ] **Step 2: Perform runtime verification**

Run `cargo run -p threadlane`; attach a project, create a supervised task, install/remove a test package, and approve/revoke a full-trust revision. Confirm task state and capability rows update without restarting the app.
