# Task 6 report

## Status

Complete. GPUI state now projects session-scoped current-context telemetry independently from cumulative usage and projects durable compaction markers into complete transcript history.

## RED

The Task 6 projection cases initially had no `ContextWindowInfo`, `AppState::active_context_window`, or `MessageRole::ContextMarker` path. During the focused test cycle, the marker reload test also exposed the required exact compact label (`742k → 118k`) rather than the generic formatter's whole-number decimal form.

Covered RED cases:

- latest manifest context versus 11.7M cumulative billed input
- marker survival and stable IDs across reload
- no fabricated marker/timestamp for legacy sessions
- model-switch estimation retaining the last persisted manifest model

## GREEN

Implemented in `app_state.rs`:

- Added `ContextWindowInfo` and a session-keyed `context_windows` projection map.
- Selected the highest-sequence main-lane manifest, with legacy request/model correlation and estimated model-limit fallback.
- Selected newest compaction telemetry, including provisional post-compaction values when its generation is newer than the manifest.
- Kept cumulative `SessionMetricsInfo` unchanged and separate from current context.
- Marked a post-manifest provider request as estimating without relabeling historical context from the model picker/new request.
- Exhausted the existing durable transcript pager to preserve complete chronological UI history, then projected markers as stable, distinct rows between message segments.
- Kept marker rows out of canonical/model transcript messages and out of assistant/tool grouping.

A minimal exhaustive-match integration in `screens/chat/view.rs` treats the new role through the existing system-row branch. It adds no new Task 7 rendering behavior.

## Verification

GREEN:

```text
cargo test -p threadlane-gpui state::app_state::tests -- --nocapture
18 passed; 0 failed

cargo check -p threadlane-gpui
PASS (existing dead-code warnings only)

git diff --check
PASS
```

The state suite includes existing complete-history/paging and exact cumulative-usage coverage.

## Self-review

- Current context is never derived from cumulative durable usage.
- Context state is keyed by session and replaced/removed through both hydration application paths.
- Persisted manifest model/limit wins; selected-but-unused UI model state is never consulted.
- `last_compacted_at` is populated only from real compaction telemetry.
- Marker content contains only telemetry counts, never internal summary content.
- Marker IDs derive from durable sequence; message IDs derive from absolute transcript position, remaining deterministic across paging and reload.
- Transcript pages are fully exhausted, preserving Task 4's complete-history contract.
- Marker boundaries flush message projection, preventing tool/reasoning grouping across a marker.

## Concerns

- Task 7 still owns the dedicated visual treatment. The compile-only role arm currently shares the existing system-row presentation.
- Existing unrelated dead-code warnings remain unchanged.
