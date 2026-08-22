# Zero-Copy Chat Row Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove deep message/activity clones from transcript row rendering and ensure expensive activity summaries and trajectory JSON are prepared once per data revision rather than on every render.

**Architecture:** Transcript rows take a cheap local `Arc<Vec<ChatMessageInfo>>` snapshot, then borrow messages and nested activities from it. A reusable iterator exposes filtered activities without allocating, projection helpers prepare activity display summaries, and the existing revision-keyed trajectory cache owns preformatted raw JSON for inspector rows.

**Tech Stack:** Rust 2024, GPUI, `gpui-component`, Serde/`serde_json`, Rust unit tests.

## Global Constraints

- Preserve transcript ordering, activity grouping, expansion behavior, and persisted JSONL formats.
- Do not introduce a second canonical message state or a new dependency.
- Keep GPUI-required owned values for callback captures, element IDs, clipboard data, and child text.
- Continue hiding `update_plan` from grouped activity rows.
- Preserve the collapsed recent-activity limit of 4.
- Invalid JSON and serialization errors must retain the existing readable fallback behavior.

## File Structure

- Modify `crates/threadlane-gpui/src/screens/chat/view.rs`: add borrowed activity traversal, remove complete-record clones, consume precomputed activity labels, and cache trajectory raw JSON by trajectory revision.
- Modify `crates/threadlane-gpui/src/state/app_state.rs`: add the display-summary projection helper and populate the display-ready field in persisted and live tool activity projections.
- Tests remain colocated in each Rust module’s existing `#[cfg(test)]` section.

---

### Task 1: Projection-Time Activity Display Summaries

**Files:**
- Modify: `crates/threadlane-gpui/src/state/app_state.rs:54-62,443-515,2550-2570`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:654-775,3420-3445`
- Test: `crates/threadlane-gpui/src/state/app_state.rs` module tests

**Interfaces:**
- Consumes: persisted activity `title`/`summary` values and live tool `name`/`arguments` values.
- Produces: `ToolActivityInfo::display_summary: String` and `fn tool_activity_display_summary(summary: &str) -> String`.

- [ ] **Step 1: Write focused failing tests for display-summary projection**

Add tests beside the existing `app_state.rs` tests:

```rust
#[test]
fn tool_activity_display_summary_is_prepared_during_projection() {
    assert_eq!(
        tool_activity_display_summary("read file · src/main.rs\nignored"),
        "read file · src/main.rs …"
    );
    assert_eq!(
        tool_activity_display_summary("still working...\nmore detail"),
        "still working..."
    );
    assert_eq!(tool_activity_display_summary(""), "");
}
```

The helper must preserve current render behavior exactly: use the first trimmed summary line and append ` …` only when another line exists and the first line does not already end in `…` or `...`.

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
cargo test -p threadlane-gpui tool_activity_display_summary_is_prepared_during_projection
```

Expected: compilation fails because `tool_activity_display_summary` does not exist.

- [ ] **Step 3: Add the display-ready projection field and helper**

Extend the view projection:

```rust
pub struct ToolActivityInfo {
    pub(crate) id: String,
    pub(crate) category: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) display_summary: String,
    pub(crate) detail: String,
    pub(crate) is_expanded: bool,
}
```

Implement `tool_activity_display_summary` next to `tool_activity_summary`. Compute the first trimmed line and append ` …` only when another line exists and the first line does not already end in `…` or `...`.

Populate `display_summary` in both constructors:

```rust
let display_summary = tool_activity_display_summary(&act.summary);
ToolActivityInfo {
    id: act.id,
    category: act.category,
    title: act.title,
    summary: act.summary,
    display_summary,
    detail: act.detail,
    is_expanded: false,
}
```

```rust
let summary = tool_activity_summary(&name, &arguments);
let display_summary = tool_activity_display_summary(&summary);
let activity = ToolActivityInfo {
    id: tool_call_id,
    category: "Working".into(),
    title: name,
    summary,
    display_summary,
    detail: arguments,
    is_expanded: false,
};
```

Update test literals to provide `display_summary`.

- [ ] **Step 4: Make rendering consume the precomputed field**

Replace the first-line parsing and multiline-ellipsis block in `render_tool_activity` with:

```rust
let display_summary = activity.display_summary.clone();
```

Retain the existing owned clones needed by GPUI children and callbacks. Do not remove `summary`; it remains canonical projected content and may be used elsewhere.

- [ ] **Step 5: Run focused and module tests**

Run:

```bash
cargo test -p threadlane-gpui tool_activity_display_summary_is_prepared_during_projection
cargo test -p threadlane-gpui screens::chat::view::tests
```

Expected: both commands pass.

- [ ] **Step 6: Commit the projection change**

```bash
git add crates/threadlane-gpui/src/state/app_state.rs crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "perf: precompute tool activity display summaries"
```

---

### Task 2: Borrow Transcript Messages and Grouped Activities

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:69-105,778-833,1914-1938`
- Test: `crates/threadlane-gpui/src/screens/chat/view.rs` module tests

**Interfaces:**
- Consumes: `&[ChatMessageInfo]` ranges represented by `TranscriptRow::Activities`.
- Produces: `fn grouped_tool_activities(messages: &[ChatMessageInfo]) -> impl Iterator<Item = &ToolActivityInfo> + Clone` and `render_activity_group(&mut self, messages: &[ChatMessageInfo], cx: &mut Context<Self>) -> AnyElement`.

- [ ] **Step 1: Write a failing ordering/filtering test for borrowed traversal**

Add this test to the existing chat view tests, using the local message factory pattern already present:

```rust
#[test]
fn grouped_tool_activities_borrows_in_order_and_hides_plan_updates() {
    let messages = vec![
        activity_message(&[("tool-1", "read_file"), ("plan", "update_plan")]),
        activity_message(&[("tool-2", "write_file")]),
    ];

    let ids = grouped_tool_activities(&messages)
        .map(|activity| activity.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["tool-1", "tool-2"]);
}
```

Implement the test-local `activity_message` factory with complete `ChatMessageInfo` and `ToolActivityInfo` values, including `display_summary` from Task 1.

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
cargo test -p threadlane-gpui grouped_tool_activities_borrows_in_order_and_hides_plan_updates
```

Expected: compilation fails because `grouped_tool_activities` does not exist.

- [ ] **Step 3: Add the allocation-free borrowed iterator**

Add beside `build_transcript_rows`:

```rust
fn grouped_tool_activities(
    messages: &[ChatMessageInfo],
) -> impl Iterator<Item = &ToolActivityInfo> + Clone {
    messages
        .iter()
        .flat_map(|message| message.tool_activities.iter())
        .filter(|activity| activity.title != "update_plan")
}
```

This iterator borrows existing records, preserves message/activity order, and performs no collection.

- [ ] **Step 4: Change activity-group rendering to consume message slices**

Change the signature to:

```rust
fn render_activity_group(
    &mut self,
    messages: &[ChatMessageInfo],
    cx: &mut Context<Self>,
) -> AnyElement
```

Inside it, derive metadata and rows from fresh iterator clones/passes:

```rust
let activities = grouped_tool_activities(messages);
let group_id = activities
    .clone()
    .next()
    .map(|activity| activity.id.clone())
    .unwrap_or_else(|| "empty".into());
let activity_count = activities.clone().count();
let hidden_count = activity_count.saturating_sub(RECENT_ACTIVITY_LIMIT);
let visible_start = if is_expanded { 0 } else { hidden_count };
let activity_rows = activities
    .skip(visible_start)
    .map(|activity| self.render_tool_activity(activity, cx))
    .collect::<Vec<_>>();
```

The final `Vec<AnyElement>` is GPUI element ownership, not a clone of activity data, and is retained.

- [ ] **Step 5: Use a local Arc snapshot in transcript row rendering**

At the start of `render_transcript_row`, clone the outer collection only:

```rust
let messages = Arc::clone(&self.transcript_messages);
```

Render rows from borrowed values:

```rust
Some(TranscriptRow::Message(message_index)) => messages
    .get(message_index)
    .map(|message| self.render_message(message, cx)),
Some(TranscriptRow::Activities(range)) => messages
    .get(range)
    .map(|messages| self.render_activity_group(messages, cx)),
```

Remove `.cloned()` from message lookup and remove the temporary collected `Vec<ToolActivityInfo>`.

- [ ] **Step 6: Run focused chat view tests**

Run:

```bash
cargo test -p threadlane-gpui grouped_tool_activities_borrows_in_order_and_hides_plan_updates
cargo test -p threadlane-gpui transcript_rows_group_consecutive_activity_only_messages
```

Expected: both tests pass and no borrow-checker error occurs.

- [ ] **Step 7: Commit zero-copy row traversal**

```bash
git add crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "perf: borrow transcript rows during rendering"
```

---

### Task 3: Cache Trajectory Raw JSON by Revision

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:108-124,883-960,1440-1465`
- Test: `crates/threadlane-gpui/src/screens/chat/view.rs` module tests

**Interfaces:**
- Consumes: the `Vec<TrajectoryEntry>` already rebuilt when `TrajectoryCacheKey` changes.
- Produces: `TrajectoryRenderCache::raw_json: Vec<String>` aligned index-for-index with `all_entries`, and `fn format_trajectory_raw_json(entry: &TrajectoryEntry) -> String`.

- [ ] **Step 1: Write a failing raw JSON formatting test**

Add a focused test using an existing `TrajectoryEntry` fixture or a minimal complete literal:

```rust
#[test]
fn trajectory_raw_json_is_prepared_for_the_revision_cache() {
    let entry = trajectory_entry_fixture();
    let raw = format_trajectory_raw_json(&entry);

    assert!(raw.contains("\"category\": \"Tool\""));
    assert!(raw.contains("\"summary\": \"Read file\""));
}
```

The fixture must initialize every current `TrajectoryEntry` field and use `TrajectoryDiagnostics::default()`.

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
cargo test -p threadlane-gpui trajectory_raw_json_is_prepared_for_the_revision_cache
```

Expected: compilation fails because `format_trajectory_raw_json` does not exist.

- [ ] **Step 3: Add formatting helper and revision-cache storage**

Add outside `render_trajectory`:

```rust
fn format_trajectory_raw_json(entry: &TrajectoryEntry) -> String {
    serde_json::to_string_pretty(entry).unwrap_or_else(|_| entry.detail.clone())
}
```

Extend the cache:

```rust
struct TrajectoryRenderCache {
    key: TrajectoryCacheKey,
    all_entries: Vec<TrajectoryEntry>,
    raw_json: Vec<String>,
    categories: Arc<Vec<String>>,
    lanes: Arc<Vec<String>>,
    lane_latest: Arc<std::collections::BTreeMap<String, String>>,
    filtered_indices: Vec<usize>,
}
```

When the key changes and `all_entries` has been projected, calculate exactly once for that revision:

```rust
let raw_json = all_entries
    .iter()
    .map(format_trajectory_raw_json)
    .collect::<Vec<_>>();
```

Store it in `TrajectoryRenderCache` alongside `all_entries`. This preserves lazy trajectory-tab behavior while removing repeated serialization from row rendering.

- [ ] **Step 4: Consume cached JSON in the Raw inspector**

When obtaining the selected entry index, also obtain the matching cached raw JSON. Replace:

```rust
let raw_json = serde_json::to_string_pretty(&entry)
    .unwrap_or_else(|_| entry.detail.clone());
```

with the matching value from `cache.raw_json`, cloned only when GPUI needs owned child text. Keep the existing fenced `json` Markdown and selectable behavior.

- [ ] **Step 5: Run focused trajectory tests**

Run:

```bash
cargo test -p threadlane-gpui trajectory_raw_json_is_prepared_for_the_revision_cache
cargo test -p threadlane-gpui trajectory_cache_key_changes_with_data_or_filter
```

Expected: both tests pass.

- [ ] **Step 6: Commit trajectory formatting cache**

```bash
git add crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "perf: cache trajectory JSON formatting"
```

---

### Task 4: Full Verification and Diff Review

**Files:**
- Verify: `crates/threadlane-gpui/src/state/app_state.rs`
- Verify: `crates/threadlane-gpui/src/screens/chat/view.rs`
- Verify: `docs/superpowers/specs/2026-08-21-zero-copy-chat-row-rendering-design.md`

**Interfaces:**
- Consumes: all prior task outputs.
- Produces: a checked, tested, whitespace-clean implementation with no accidental generated files.

- [ ] **Step 1: Run the GPUI test suite**

```bash
cargo test -p threadlane-gpui
```

Expected: all `threadlane-gpui` tests pass.

- [ ] **Step 2: Run the required GPUI compile check**

```bash
cargo check -p threadlane-gpui
```

Expected: command exits successfully. Existing unrelated warnings may remain.

- [ ] **Step 3: Check patch whitespace**

```bash
git diff --check HEAD~3
```

Expected: no output and exit status 0.

- [ ] **Step 4: Review the final change set**

```bash
git status --short
git diff --stat HEAD~3
git diff HEAD~3 -- crates/threadlane-gpui/src/state/app_state.rs crates/threadlane-gpui/src/screens/chat/view.rs
```

Confirm that complete-record `.cloned()` calls and the temporary `Vec<ToolActivityInfo>` are gone from `render_transcript_row`, JSON serialization is absent from the Raw inspector render branch, persistence types are unchanged, and no generated files are present.

- [ ] **Step 5: Commit verification-only fixes if required**

If verification required a code correction, stage only the two touched Rust files and commit it:

```bash
git add crates/threadlane-gpui/src/state/app_state.rs crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "fix: preserve chat row rendering behavior"
```

If no corrections were necessary, do not create an empty commit.
