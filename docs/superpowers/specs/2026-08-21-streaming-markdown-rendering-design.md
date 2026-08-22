# Streaming Markdown Rendering Design

## Summary

Render assistant content as Markdown throughout streaming instead of showing plain text until generation completes. Apply the same behavior to reasoning content only while its existing collapsible panel is expanded. Reuse the cached `TextViewState` instances and their append-aware update path so the UX improvement does not require reparsing the full message for every delta.

## Problem

`should_use_markdown(streaming)` currently disables Markdown whenever an assistant message is streaming. This affects both the visible assistant response and expanded reasoning. Users see raw Markdown syntax during generation, followed by a potentially disruptive layout change when streaming ends and Markdown rendering is enabled.

The current Markdown cache updates an existing `TextViewState` with `set_text` whenever content length changes. Model output is normally append-only, but `set_text` uses the full-replacement path rather than the component's incremental append path.

## Goals

- Render visible assistant output as Markdown while it streams.
- Avoid the plain-text-to-Markdown transition when streaming finishes.
- Render streaming reasoning as Markdown when the user expands it.
- Avoid parsing or rendering collapsed reasoning.
- Use the existing Markdown component and cache rather than adding a parallel renderer.
- Preserve correctness if message content changes in a non-append-only way.

## Non-goals

- Eagerly parse collapsed reasoning.
- Introduce a new Markdown parser or rendering framework.
- Add speculative timers, queues, or custom throttling before profiling identifies a need.
- Change the visual design of assistant messages or reasoning panels.
- Change stream ingestion, persistence, or provider behavior.

## UX Behavior

### Assistant responses

Assistant content uses Markdown from the first visible streaming update through completion. Headings, lists, emphasis, links, and code blocks therefore use one consistent presentation during the response instead of changing format at the end.

Incomplete Markdown constructs may temporarily render according to the parser's best interpretation. This is preferable to exposing all Markdown source until completion, and subsequent deltas will update the rendered structure.

### Reasoning

Reasoning remains collapsed by default. While collapsed, no reasoning `TextView` is built and no Markdown update is performed by the chat view. If the user expands reasoning during generation, its visible content renders and updates as Markdown. This keeps rendering cost aligned with user-visible content while making expanded reasoning consistent with the assistant response.

## Architecture and Data Flow

The change remains within the existing chat view and its per-message Markdown state cache.

1. A stream delta updates `ChatMessageInfo.content` or `reasoning_content` in application state.
2. The chat view rerenders the affected transcript row.
3. Visible assistant content obtains a cached Markdown state keyed by message ID.
4. Expanded reasoning obtains a separate cached Markdown state keyed by its existing reasoning-specific key.
5. The view compares the current source with the source represented by the cached state:
   - If the current source extends the cached source, append only the suffix using `TextViewState::push_str`.
   - If it does not extend the cached source, replace the full value using `TextViewState::set_text`.
   - If unchanged, perform no state update.
6. `TextView` renders the cached Markdown state during streaming and after completion.

The cache must track enough source information to verify prefix compatibility. Tracking only byte length is insufficient because different content can have the same length and a longer value does not necessarily preserve the previous prefix. The simplest correct representation is the last source string alongside the `TextViewState`. Because the message already owns another copy of this content, this memory trade-off is accepted for correctness and implementation simplicity. A future optimization may replace it with a revision or stronger append guarantee if profiling demonstrates a need.

## Component Boundaries

### Shared Markdown-state update helper

Introduce or extract one focused helper in the chat view that updates a cached Markdown entry from new source text. It is responsible for choosing append, replacement, or no-op behavior. Both assistant content and expanded reasoning use this helper so they cannot drift into different update semantics.

### Assistant message renderer

Remove the streaming plain-text branch. Non-empty assistant content always renders through the cached Markdown state.

### Reasoning renderer

Retain the existing lazy `is_expanded` boundary. Inside that boundary, remove the streaming plain-text branch and use the same cached Markdown update helper.

## Performance Strategy

The initial optimization is incremental parsing, not deferred formatting:

- Reuse one `TextViewState` per visible message or expanded reasoning block.
- Use `push_str` for append-only stream updates. `gpui-component` routes this through its append-compatible parse mode.
- Use `set_text` only as a correctness fallback.
- Continue relying on the application's existing frame-budgeted stream drain.
- Continue relying on `TextViewState` revision handling and background parsing behavior.

No custom throttling is included initially. If profiling later shows frame latency from unusually frequent deltas or very large messages, updates can be coalesced to a frame or short interval without reverting to plain-text streaming. That optimization should be evidence-driven.

## Error and Edge-Case Handling

- Empty assistant content does not create a Markdown view.
- Collapsed reasoning does not create or update a reasoning Markdown view.
- Expanding reasoning after multiple deltas initializes the state from the complete reasoning accumulated so far.
- A source prefix mismatch triggers full replacement, covering restored content, edits, truncation, or any future non-append stream behavior.
- Completion does not swap rendering implementations, preventing final-format layout churn.
- Session changes continue clearing the existing Markdown cache.
- Incomplete fenced code blocks and other partial constructs are allowed to update naturally as more source arrives.

## Testing

Add focused unit coverage for the update decision independent of GPUI rendering:

- Empty cached source followed by content chooses append or initialization correctly.
- A strict append chooses the incremental path and identifies the exact suffix.
- Identical content chooses no update.
- Same-length changed content chooses replacement.
- Longer content with a different prefix chooses replacement.
- Shorter content chooses replacement.

Update the existing streaming policy test so it asserts Markdown is enabled for streaming and completed content. If practical within existing test facilities, cover that reasoning remains gated by expansion through the renderer's extracted policy/helper rather than attempting brittle pixel-level tests.

Run:

```bash
cargo test -p threadlane-gpui hot_path_tests
cargo check -p threadlane-gpui
git diff --check
```

## Success Criteria

- Streaming assistant text is rendered as Markdown from its first visible update.
- Expanded streaming reasoning is rendered as Markdown.
- Collapsed reasoning incurs no Markdown view update in the chat renderer.
- Append-only deltas use `TextViewState::push_str` rather than replacing the entire source.
- Non-append mutations remain correct through a full-replacement fallback.
- Existing chat behavior, selection, copy actions, transcript virtualization, and session switching remain intact.
