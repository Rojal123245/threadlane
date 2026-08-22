# Streaming Markdown Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render assistant responses and user-expanded reasoning as Markdown throughout streaming while updating cached Markdown incrementally for append-only deltas.

**Architecture:** Keep rendering in the existing `ChatListView` and replace its length-only Markdown cache entries with entries that retain the last source plus the existing `TextViewState`. A shared cache-update method classifies each new value as unchanged, append-only, or replacement, uses `TextViewState::push_str` for the common streaming path, and falls back to `set_text` for correctness.

**Tech Stack:** Rust, GPUI, `gpui-component::text::{TextView, TextViewState}`, Cargo tests.

## Global Constraints

- Reuse the existing `TextViewState` Markdown renderer and `ChatListView::markdown_states` cache; add no dependency or parallel renderer.
- Render assistant content as Markdown during and after streaming.
- Render reasoning Markdown only inside the existing expanded-detail boundary; collapsed reasoning must not create or update a reasoning Markdown view.
- Use append-aware updates when the new source preserves the cached prefix and full replacement for truncation or prefix mismatch.
- Do not add custom timers, queues, eager reasoning parsing, or speculative throttling.
- Preserve message selection, copy actions, streaming animation, transcript virtualization, and session cache clearing.
- Keep the change focused in `crates/threadlane-gpui/src/screens/chat/view.rs`.

---

## File Structure

- Modify `crates/threadlane-gpui/src/screens/chat/view.rs`: define the Markdown update classifier and cache entry, centralize cached-state updates, use Markdown for streaming assistant/reasoning content, and add focused hot-path tests.
- No new production files: the behavior belongs to the existing chat view and splitting it out would create an unnecessary abstraction for one consumer.

### Task 1: Add correct incremental Markdown cache updates

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:65-67`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:160`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:169-340`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:2049-2066`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:2160-2184`
- Test: `crates/threadlane-gpui/src/screens/chat/view.rs:3407-3421`

**Interfaces:**
- Consumes: `TextViewState::markdown(&str, &mut Context<_>)`, `TextViewState::push_str(&str, &mut Context<_>)`, and `TextViewState::set_text(&str, &mut Context<_>)` from the existing `gpui-component` dependency.
- Produces: `fn classify_markdown_update<'a>(current: &str, next: &'a str) -> MarkdownUpdate<'a>` and `ChatListView::markdown_state(&mut self, key: String, source: &str, cx: &mut Context<Self>) -> Entity<TextViewState>` for both assistant and reasoning render paths.

- [ ] **Step 1: Replace the obsolete streaming-policy test with failing update-classification tests**

In `hot_path_tests`, replace the `should_use_markdown` import and `markdown_is_deferred_until_streaming_completes` test with the following import and tests:

```rust
use super::{
    build_transcript_rows, classify_markdown_update, format_trajectory_raw_json,
    grouped_tool_activities, MarkdownUpdate, TrajectoryCacheKey, TrajectoryMode, TranscriptRow,
};

#[test]
fn markdown_update_appends_only_the_new_suffix() {
    assert_eq!(
        classify_markdown_update("Hello", "Hello **world**"),
        MarkdownUpdate::Append(" **world**")
    );
}

#[test]
fn markdown_update_skips_identical_content() {
    assert_eq!(
        classify_markdown_update("Hello", "Hello"),
        MarkdownUpdate::Unchanged
    );
}

#[test]
fn markdown_update_replaces_non_append_changes() {
    assert_eq!(
        classify_markdown_update("Hello", "Jello"),
        MarkdownUpdate::Replace
    );
    assert_eq!(
        classify_markdown_update("Hello", "Hello there"),
        MarkdownUpdate::Append(" there")
    );
    assert_eq!(
        classify_markdown_update("Hello there", "Hello"),
        MarkdownUpdate::Replace
    );
    assert_eq!(
        classify_markdown_update("Hello", "Hello!"),
        MarkdownUpdate::Append("!")
    );
}
```

The same-length assertion covers changed content, the longer-prefix-mismatch case must also be explicit, so add it to `markdown_update_replaces_non_append_changes`:

```rust
assert_eq!(
    classify_markdown_update("Hello", "Jello there"),
    MarkdownUpdate::Replace
);
```

- [ ] **Step 2: Run the focused tests and verify they fail for the missing symbols**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests::markdown_update -- --nocapture
```

Expected: compilation fails because `classify_markdown_update` and `MarkdownUpdate` do not exist yet. The failure establishes that the new behavior is not already implemented.

- [ ] **Step 3: Add the update classifier and source-aware cache entry**

Keep `should_use_markdown` unchanged and add these definitions immediately after it near the existing transcript helpers:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MarkdownUpdate<'a> {
    Unchanged,
    Append(&'a str),
    Replace,
}

fn classify_markdown_update<'a>(current: &str, next: &'a str) -> MarkdownUpdate<'a> {
    if current == next {
        MarkdownUpdate::Unchanged
    } else if let Some(suffix) = next.strip_prefix(current) {
        MarkdownUpdate::Append(suffix)
    } else {
        MarkdownUpdate::Replace
    }
}

struct MarkdownRenderState {
    source: String,
    state: Entity<TextViewState>,
}
```

Change the cache field from:

```rust
markdown_states: HashMap<String, (usize, Entity<TextViewState>)>,
```

to:

```rust
markdown_states: HashMap<String, MarkdownRenderState>,
```

- [ ] **Step 4: Run the focused classifier tests and verify they pass**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests::markdown_update -- --nocapture
```

Expected: all three `markdown_update_*` tests pass.

- [ ] **Step 5: Add one shared method that initializes, appends, or replaces cached Markdown state**

Add this method inside `impl ChatListView`, immediately before `render_reasoning_block`:

```rust
fn markdown_state(
    &mut self,
    key: String,
    source: &str,
    cx: &mut Context<Self>,
) -> Entity<TextViewState> {
    let entry = self
        .markdown_states
        .entry(key)
        .or_insert_with(|| MarkdownRenderState {
            source: source.to_owned(),
            state: cx.new(|cx| TextViewState::markdown(source, cx)),
        });

    match classify_markdown_update(&entry.source, source) {
        MarkdownUpdate::Unchanged => {}
        MarkdownUpdate::Append(suffix) => {
            entry.source.push_str(suffix);
            entry
                .state
                .update(cx, |state, cx| state.push_str(suffix, cx));
        }
        MarkdownUpdate::Replace => {
            entry.source.clear();
            entry.source.push_str(source);
            entry
                .state
                .update(cx, |state, cx| state.set_text(source, cx));
        }
    }

    entry.state.clone()
}
```

This method stores the actual cached source rather than only its byte length, making prefix verification correct for same-length edits and longer non-prefix replacements.

- [ ] **Step 6: Route existing completed-message Markdown through the shared method**

Inside the assistant content branch, replace the direct `markdown_states.entry(...)` block with:

```rust
let markdown_state =
    self.markdown_state(msg.id.clone(), &msg.content, cx);
let markdown = TextView::new(&markdown_state).selectable(true);
```

Inside the expanded reasoning detail, replace the direct `markdown_states.entry(...)` block with:

```rust
let markdown_state = self.markdown_state(
    format!("reasoning-{}", msg.id),
    reasoning,
    cx,
);
container
    .child(TextView::new(&markdown_state).selectable(true))
    .into_any_element()
```

At this point, retain the existing `should_use_markdown` helper and both guards unchanged; Task 2 changes and then removes that streaming policy.

- [ ] **Step 7: Run focused tests and the GPUI type check**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests -- --nocapture
cargo check -p threadlane-gpui
```

Expected: all hot-path tests pass and `cargo check` exits successfully. Existing unrelated warnings may remain, but there must be no new errors or warnings caused by this change.

- [ ] **Step 8: Commit the incremental cache change**

```bash
git add crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "perf(gpui): update cached markdown incrementally"
```

### Task 2: Enable Markdown for visible streaming content

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:65-67`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:2049-2066`
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs:2160-2184`
- Test: `crates/threadlane-gpui/src/screens/chat/view.rs:3407-3450`

**Interfaces:**
- Consumes: `ChatListView::markdown_state(String, &str, &mut Context<Self>) -> Entity<TextViewState>` from Task 1.
- Produces: one Markdown rendering path for assistant content regardless of `msg.streaming`, plus lazy Markdown rendering for reasoning only inside `is_expanded.then(...)`.

- [ ] **Step 1: Add a failing policy test for streaming Markdown visibility**

Add this temporary policy test to `hot_path_tests` and include `should_use_markdown` in the module imports:

```rust
#[test]
fn markdown_is_used_during_and_after_streaming() {
    assert!(should_use_markdown(true));
    assert!(should_use_markdown(false));
}
```

- [ ] **Step 2: Run the policy test and verify the streaming assertion fails**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests::markdown_is_used_during_and_after_streaming -- --nocapture
```

Expected: the test fails at `assert!(should_use_markdown(true))`, proving streaming content is still routed to plain text.

- [ ] **Step 3: Enable Markdown while streaming with the minimal policy change**

Change the helper to:

```rust
fn should_use_markdown(_streaming: bool) -> bool {
    true
}
```

- [ ] **Step 4: Run the policy and cache tests**

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests::markdown_ -- --nocapture
```

Expected: `markdown_is_used_during_and_after_streaming` and all `markdown_update_*` tests pass.

- [ ] **Step 5: Remove the now-obsolete policy branches and helper while preserving lazy reasoning**

Delete `should_use_markdown` and its temporary test/import. This is a behavior-preserving cleanup after the passing policy test; leaving a constant policy helper and unreachable plain-text branches would obscure the final design.

In `render_reasoning_block`, keep all Markdown work inside the existing lazy closure:

```rust
let detail = is_expanded.then(|| {
    let container = div()
        .ml(px(26.0))
        .mt_1()
        .p_2()
        .max_h(px(300.0))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .text_xs()
        .text_color(theme.muted_foreground)
        .overflow_y_scrollbar();
    let markdown_state = self.markdown_state(
        format!("reasoning-{}", msg.id),
        reasoning,
        cx,
    );
    container
        .child(TextView::new(&markdown_state).selectable(true))
        .into_any_element()
});
```

In the assistant branch, replace the plain/Markdown conditional with one path:

```rust
let markdown_state =
    self.markdown_state(msg.id.clone(), &msg.content, cx);
let content_element = div()
    .w_full()
    .text_sm()
    .text_color(theme.foreground)
    .child(TextView::new(&markdown_state).selectable(true))
    .into_any_element();
```

Keep the surrounding `if !msg.content.is_empty()` guard and the existing streaming opacity animation unchanged.

- [ ] **Step 6: Run formatting and focused validation**

Run:

```bash
cargo fmt --all -- --check
cargo test -p threadlane-gpui hot_path_tests -- --nocapture
cargo check -p threadlane-gpui
git diff --check
```

Expected: formatting is clean, all hot-path tests pass, `cargo check` succeeds, and `git diff --check` reports no whitespace errors.

- [ ] **Step 7: Inspect the final diff for scope and lazy reasoning behavior**

Run:

```bash
git diff -- crates/threadlane-gpui/src/screens/chat/view.rs
```

Confirm all of the following in the diff:

- There is no streaming plain-text assistant branch.
- `markdown_state` is called for reasoning only inside `is_expanded.then(...)`.
- The existing streaming opacity animation remains present.
- Session changes still call `self.markdown_states.clear()`.
- No stream ingestion, persistence, provider, or dependency files changed.

- [ ] **Step 8: Commit streaming Markdown rendering**

```bash
git add crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "feat(gpui): render markdown while streaming"
```

### Task 3: Final regression verification

**Files:**
- Verify: `crates/threadlane-gpui/src/screens/chat/view.rs`
- Verify: `docs/superpowers/specs/2026-08-21-streaming-markdown-rendering-design.md`

**Interfaces:**
- Consumes: the completed incremental Markdown cache and unified streaming renderer from Tasks 1 and 2.
- Produces: verification evidence that the implementation meets the approved design and repository checks.

- [ ] **Step 1: Run the required GPUI checks from the repository root**

```bash
cargo test -p threadlane-gpui hot_path_tests -- --nocapture
cargo check -p threadlane-gpui
git diff --check
```

Expected: all commands exit with status 0. Report unrelated pre-existing warnings exactly rather than claiming a warning-free build.

- [ ] **Step 2: Verify repository state and commit history**

```bash
git status --short
git log -3 --oneline
```

Expected: no uncommitted implementation changes remain. The recent history contains the design commit, incremental-cache commit, and streaming-Markdown commit.

- [ ] **Step 3: Report behavior and verification accurately**

The completion report must mention:

- Streaming assistant responses now use Markdown.
- Expanded streaming reasoning uses Markdown, while collapsed reasoning remains lazy.
- Append-only updates use `push_str`; mismatches use `set_text`.
- The exact test/check commands that passed.
- No claim of visual verification unless the application was actually run and observed.
