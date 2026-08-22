# Task 4 Report

## Status

DONE

## Commit

- `2ba474975e86e187001ba5764c5484c212687de8`
- `feat(harness): record context compaction telemetry`

## Changed files

- `crates/threadlane-runtime/src/harness/types.rs`
  - Added `CompactionReason` and its stable snake-case/string representation.
  - Added additive `Record::ContextCompacted` telemetry.
  - Added defaulted/conditionally serialized context metadata to `ContextManifestCaptured`.
  - Included the new record in sequence rewriting and record identity/lane/run accessors.
- `crates/threadlane-runtime/src/harness/procedure.rs`
  - Added `CompactionProcedure::checkpoint_open_run`, which validates the active run, appends an internal replacement summary, moves the lane leaf, and deliberately leaves the foreground operation open.
  - Added idle/wrong-run and successful checkpoint tests.
- `crates/threadlane-runtime/src/harness/reducer.rs`
  - Handles `ContextCompacted` exhaustively as observational telemetry without changing lane reduction.
- `crates/threadlane-runtime/src/harness/trajectory.rs`
  - Handles the additive telemetry exhaustively without changing trajectory semantics.
- `crates/threadlane-runtime/src/harness/jsonl.rs`
  - Added schema compatibility and telemetry round-trip tests.
  - Added `ContextCompactedMarker`, `TranscriptItem`, and `TranscriptPage.items`.
  - Pages messages and markers in durable order, counts only messages toward page size, excludes internal `compaction_summary` entries, and retains legacy fixture support.
  - Classifies compaction telemetry with the other observational/data-sync records.
- `crates/threadlane-runtime/src/harness/mod.rs`
  - Exports the new reason, marker, and transcript item types.
- `crates/threadlane-session/src/coding_agent/harness.rs`
  - Minimal compile/schema integration: persists the model, context limit/estimate flag, and compaction generation already present on `ProviderTraceEvent::ContextManifest` into the newly extended harness record.
- `crates/threadlane-gpui/src/state/app_state.rs`
  - Minimal compile integration for the `TranscriptPage.items` API. Existing history projection continues to project message items and leaves durable marker rendering to the later GPUI telemetry task.

## RED evidence available from recovery

The timed-out implementer's original pre-implementation RED command output was not present in the recovered working tree/session evidence. The recovered tree already contained the three new tests and most production implementation, so rerunning the intended Step 2 state could not truthfully reproduce the original missing-symbol failures without discarding valid work.

The first recovery compile did expose an incomplete migration RED state:

- `cargo test -p threadlane-runtime harness --no-fail-fast`
- Failed with seven `E0615` errors because legacy JSONL tests still accessed removed `TranscriptPage.messages` as a field after the production API had changed to `TranscriptPage.items`.
- Those tests were migrated to their test-only message projection, preserving their existing assertions while the production API remains exactly item-based.

## GREEN commands and output

Required focused RED-test names after completion:

- `cargo test -p threadlane-runtime legacy_context_manifest_deserializes_without_new_metadata -- --nocapture` — PASS, 1 passed.
- `cargo test -p threadlane-runtime compaction_telemetry_round_trips -- --nocapture` — PASS, 1 passed.
- `cargo test -p threadlane-runtime transcript_page_orders_compaction_marker_without_exposing_summary -- --nocapture` — PASS, 1 passed.

Required Step 6 checks:

- `cargo test -p threadlane-runtime harness::jsonl::tests -- --nocapture` — PASS, 17 passed, 0 failed.
- `cargo test -p threadlane-runtime harness::procedure::tests -- --nocapture` — PASS, 2 passed, 0 failed.
- `cargo test -p threadlane-runtime harness::trajectory::tests -- --nocapture` — PASS, 4 passed, 0 failed.
- `cargo test -p threadlane-runtime` — PASS, 85 passed, 0 failed; 2 perf tests and 2 doctests ignored.

Supplementary integration checks:

- `cargo test -p threadlane-session` — PASS, 111 passed, 0 failed; one pre-existing unused-import warning in `supervisor.rs`.
- `cargo check -p threadlane-gpui` — PASS; pre-existing dead-code warnings only.
- `git diff --check` — PASS.
- Touched Rust files were formatted directly with `rustfmt --edition 2021` during recovery.

`cargo fmt --all -- --check` was also attempted. It reported unrelated pre-existing formatting drift in `threadlane-gpui/src/state/app_state.rs` (a blank line outside this task's integration hunk) and `threadlane-gpui/src/state/mod.rs` (cfg import ordering). Those unrelated lines were not included in this task.

## Self-review

- Confirmed legacy manifests deserialize with absent metadata and reserialize without injecting optional/default-valued fields.
- Confirmed `ContextCompacted` round-trips all required telemetry fields.
- Confirmed the reducer treats compaction telemetry as observational and does not mutate open-operation or leaf state.
- Confirmed checkpoint effects use unique subsequent sequences, reject missing/mismatched open runs, replace canonical model context with the internal summary, move the leaf, and keep the run open.
- Confirmed transcript paging preserves byte/sequence order, excludes internal summaries, emits main-lane markers, and counts only message items toward the requested minimum.
- Confirmed session and GPUI compatibility edits are minimal consequences of the additive manifest fields and the intentional `TranscriptPage.items` replacement.
- Reviewed the final diff for unrelated dependency, generated-file, and state-path changes; none were added.

## Concerns

- Original strict RED output is unavailable because this was a recovery from an already partially implemented tree; the report records the actual incomplete-migration compile failure rather than inventing evidence.
- Full-workspace rustfmt currently has pre-existing drift outside this task. Required tests, supplementary consumer checks, and `git diff --check` all pass.

## Review-finding correction

A focused scope correction was completed after review:

- Removed the premature GPUI paged-history state and behavior (`HistoryPageResult`, transcript cursor/file/page serial state, older-page loading, and newest-page-only hydration).
- Restored complete durable transcript loading for startup, session switching, and refresh hydration. The GPUI projection remains message-only until the later task can integrate `ContextCompacted` markers coherently rather than adding and immediately discarding a premature marker projection.
- Added `session_messages_include_complete_durable_history`, covering a 45-message history (larger than the former 40-message page) from first through last message.
- Reverted the unrelated open-operation-specific parent selection in `CodingSessionHarness::append_message_inner` and removed its behavior-specific test. No Task 4 telemetry/checkpoint requirement depends on that parent-selection policy.
- Preserved the core Task 4 telemetry, checkpoint, JSONL paging, and transcript-marker implementation.

### Correction validation

- `cargo test -p threadlane-runtime harness::jsonl::tests -- --nocapture` — PASS (17 passed).
- `cargo test -p threadlane-runtime harness::procedure::tests -- --nocapture` — PASS (4 passed).
- `cargo test -p threadlane-runtime harness::trajectory::tests -- --nocapture` — PASS (no matching unit tests; command passed).
- `cargo test -p threadlane-runtime` — PASS (85 passed, 2 benchmark tests ignored, 2 doc tests ignored).
- `cargo test -p threadlane-session coding_agent::harness::tests -- --nocapture` — PASS (18 passed).
- `cargo test -p threadlane-session` — PASS (110 passed); pre-existing unused-import warning in `supervisor.rs` only.
- `cargo test -p threadlane-gpui state::app_state::tests::session_messages_include_complete_durable_history -- --nocapture` — PASS (1 passed).
- `cargo test -p threadlane-gpui state::app_state::tests` — PASS (14 passed); pre-existing dead-code warnings only.
- `cargo check -p threadlane-gpui` — PASS; pre-existing dead-code warnings only.
- `cargo fmt -- <touched Rust files>` — PASS.
- `git diff --check` — PASS.
