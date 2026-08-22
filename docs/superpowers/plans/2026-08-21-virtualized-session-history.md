# Virtualized Session History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render only visible chat rows and page backward through canonical session JSONL without full-file parsing or reduction.

**Architecture:** Add an opaque backward byte cursor and page reader to the runtime JSONL module, then switch GPUI history hydration to that API. Replace the transcript scroll container with GPUI's existing variable-height `list` and `ListState`, retaining the current tool-activity grouping through cached row descriptors.

**Tech Stack:** Rust, serde/serde_json, std::fs seek/read, GPUI `list`/`ListState`, existing background executor.

**Spec:** `docs/superpowers/specs/2026-08-21-virtualized-session-history-design.md`

## Global Constraints

- Do not add dependencies or a persistent index sidecar.
- Keep canonical JSONL durability and full `JsonlStore` validation unchanged.
- Keep filesystem reads and projection work off the GPUI thread.
- Preserve torn-tail behavior and existing chat/tool/reasoning presentation.

---

### Task 1: Backward transcript page reader

**Files:**
- Modify: `crates/threadlane-runtime/src/harness/jsonl.rs`
- Modify: `crates/threadlane-runtime/src/harness/mod.rs`

**Interfaces:**
- Produces: `TranscriptCursor`, `TranscriptPage`, and `read_transcript_page(path, cursor, minimum_messages)`.

- [ ] Write runtime tests proving newest-first demand paging, cursor continuation, user-boundary expansion, legacy-node support, and torn-tail handling.
- [ ] Run the focused tests and verify they fail because the page API is absent.
- [ ] Implement a bounded reverse chunk scanner using `File`, `Seek`, and `Read`; parse only scanned complete lines into the existing `SessionLine` representation.
- [ ] Run `cargo test -p threadlane-runtime transcript_page` and the complete runtime suite.
- [ ] Commit the runtime page-reader slice.

### Task 2: GPUI demand-page integration

**Files:**
- Modify: `crates/threadlane-gpui/src/state/app_state.rs`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs`
- Modify: `crates/threadlane-gpui/src/screens/workspace/view.rs`

**Interfaces:**
- Consumes: runtime `read_transcript_page` and opaque cursor.
- Produces: background initial/tail hydration and older-page requests that never call `JsonlStore::open_read_only` for transcript messages.

- [ ] Write state tests proving the newest page and older continuation use cursors and produce unique page-prefixed message IDs.
- [ ] Run the focused GPUI tests and verify the missing cursor behavior fails.
- [ ] Replace history index fields and page functions with cursor-backed requests while leaving full trajectory/diagnostic hydration asynchronous.
- [ ] Run the focused GPUI state tests and `cargo check -p threadlane-gpui`.
- [ ] Commit the demand-page integration.

### Task 3: Variable-height chat virtualization

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs`

**Interfaces:**
- Consumes: `Arc<Vec<ChatMessageInfo>>` and history cursor state.
- Produces: cached transcript row descriptors rendered through `gpui::list` and `ListState`.

- [ ] Write unit tests for stable activity grouping plus append/prepend/reset row transitions.
- [ ] Run the focused tests and verify the virtual row-state API is absent.
- [ ] Add `ListState` with bottom alignment and tail following, cache row descriptors, render through `cx.processor`, preserve the top anchor on prepend, and remeasure only the streaming tail row.
- [ ] Run focused view tests and `cargo check -p threadlane-gpui`.
- [ ] Commit the virtualization slice.

### Task 4: Verification

**Files:**
- Modify only files required by failures caused by this work.

**Interfaces:**
- Consumes: all preceding slices.
- Produces: verified implementation and measured bounded page reads.

- [ ] Run `cargo fmt --all -- --check` and `git diff --check`.
- [ ] Run `cargo test -p threadlane-runtime`, focused GPUI tests, and `cargo check -p threadlane-gpui`.
- [ ] Run `cargo test --workspace`; report any unrelated environment-dependent fixture separately.
- [ ] Audit the diff and confirm no generated files or unrelated user changes were included.

