# Restore Normal Activity Rail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore normal thinking and tool activity as individual collapsible rows while preserving the subagent rail.

**Architecture:** Remove the presentation-only grouping of ordinary `ChatMessage::Thinking` and non-subagent `ChatMessage::Tool` entries. Route persisted messages through the existing `ThinkingMsg` and `ToolMsg` templates, and give streaming thinking its own display-row variant so it also uses `ThinkingMsg`.

**Tech Stack:** Rust, Makepad `script_mod!`, existing `ChatList`/`ToolFoldHeader` widgets.

## Global Constraints

- Reuse the existing `ThinkingMsg`, `ToolMsg`, `SubagentMsg`, and `SubagentRail` widgets.
- Do not change subagent grouping, harness lifecycle merging, persistence, or provider behavior.
- Do not add dependencies or new widget abstractions.

---

### Task 1: Restore individual normal activity rows

**Files:**
- Modify: `crates/threadlane/src/panels/chat/view.rs:75-290`
- Modify: `crates/threadlane/src/panels/chat/view.rs:944-1148`
- Test: `crates/threadlane/src/panels/chat/view.rs:1413-1431`
- Modify: `crates/threadlane/src/app/mod.rs:947-986` only if `ActivityGroupMsg` becomes unused

**Interfaces:**
- Consumes: `ChatMessage`, `StreamingKind`, existing `DisplayRow::Message`, `DisplayRow::Tool`, `ThinkingMsg`, and `ToolMsg`.
- Produces: one display row per normal thinking/tool message and a streaming-thinking row rendered with `ThinkingMsg`.

- [ ] **Step 1: Write the failing regression test**

Replace the grouping assertion with a test that builds thinking, read-tool, edit-tool, and assistant messages, then asserts four rows: `Message(0)`, two `Tool` rows, and `Message(3)`.

- [ ] **Step 2: Run the focused test and verify RED**

Run `cargo test -p threadlane panels::chat::view::tests::consecutive_activity_messages_keep_individual_rows`.

Expected: FAIL because the current code returns one `ActivityGroup` plus the assistant row.

- [ ] **Step 3: Implement the minimum routing change**

In `display_rows_with_harness`, stop collecting ordinary activity messages into `InterimRow::ActivityGroup`. Preserve subagent child filtering, then emit `InterimRow::Message(message_index)` for each persisted message. Map normal tool messages through the existing `DisplayRow::Tool` branch and thinking messages through `DisplayRow::Message`.

Represent non-empty streaming thinking separately and render it through `ThinkingMsg` using `streaming_text`; keep streaming assistant rendering unchanged. Delete `CachedActivityGroup`, `ActivityCounts`, `activity_detail`, and `ActivityGroupMsg` only after confirming they have no remaining callers.

- [ ] **Step 4: Run the focused chat tests and verify GREEN**

Run `cargo test -p threadlane panels::chat::view::tests`.

Expected: all chat-view tests pass, including existing subagent rail tests.

- [ ] **Step 5: Verify compilation and patch hygiene**

Run `cargo check -p threadlane` and `git diff --check`.

Expected: both commands exit successfully.

- [ ] **Step 6: Verify in a fresh Makepad Studio build**

Clear the prior Threadlane Studio build, run `threadlane` again, and inspect the transcript. Confirm normal thinking and tool steps appear as separate collapsible rows and delegated work still appears in `SubagentRail`.

- [ ] **Step 7: Commit only the implementation files**

Stage `crates/threadlane/src/panels/chat/view.rs` and `crates/threadlane/src/app/mod.rs`, then commit with message `fix: restore normal activity rail`.
