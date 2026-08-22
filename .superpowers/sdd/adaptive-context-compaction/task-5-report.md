# Task 5 Report — Durable Session Provider-Boundary Compaction

## Status

Implemented durable provider-boundary context preparation and compaction. Compaction checkpoints, retained-tail entries, canonical re-projection, and additive telemetry are committed before provider-start persistence and network work. The foreground run remains open throughout.

## RED

The initial focused commands selected zero tests because the required Task 5 fixtures were not present. Code inspection also established the behavioral gaps the fixtures target:

- durable runs installed provider trace/message callbacks but no provider-boundary preparer;
- prompt-only auto-compaction ran before durable prompt acceptance rather than between provider attempts;
- the session harness had no open-run checkpoint/commit integration or `ContextCompacted` commit path;
- manual harness compaction did not emit `CompactionReason::Manual` telemetry;
- there was no cancellation fixture asserting that no compaction/provider start follows an accepted abort.

The five named fixtures and an explicit cancellation fixture were then added before final verification.

## GREEN

Implemented:

- `CodingSessionHarness::prepare_provider_boundary` using the effective attempt model and tool schema.
- Normal and one strict retained-tail attempt, with terminal error after the bounded retry.
- One overflow-recovery compaction path, selected by `ProviderBoundaryRequest::overflow_recovery`.
- `checkpoint_open_run_compaction`, backed by `CompactionProcedure::checkpoint_open_run`, so no nested operation is started and the foreground run is not finished.
- Durable commit order: checkpoint and lane move, gated effect completion, retained-tail append, canonical projection read, then additive `ContextCompacted` telemetry.
- Main-lane committed generation recovery via maximum durable generation.
- Cancellation check before accepting compaction effects; accepted gated effects are driven synchronously to completion.
- Shared preparer installation only as part of accepted/adopted durable run recorder installation, and removal during run teardown.
- Removal of prompt-only durable auto-compaction after the boundary hook was active.
- Manual compaction telemetry with `CompactionReason::Manual`.
- Append-only reload behavior: canonical model context uses checkpoint plus retained tail while transcript projection retains original entries.

Locking review:

- The shared Tokio mutex is held only around synchronous journal freshness, gated persistence, and projection work.
- No harness lock is held during provider network work, tool callbacks, or other expensive asynchronous callbacks.
- Provider trace, message, tool, and permission callbacks retain their existing short independent lock scopes.

## Tests

Focused fixtures (all PASS, one test selected each):

- `adaptive_compaction_commits_before_next_provider_attempt`
- `reload_uses_checkpoint_tail_but_transcript_keeps_original_entries`
- `compaction_persistence_failure_blocks_provider`
- `ineffective_compaction_retries_once`
- `provider_overflow_retries_once`
- `cancellation_before_compaction_has_no_partial_operation_or_provider_start`

Full verification:

- `cargo test -p threadlane-session` — PASS, 116 tests.
- `cargo check -p threadlane-session` — PASS.
- `cargo check -p threadlane-gpui` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.

Existing unrelated warnings remain in `threadlane-session::supervisor` and dead-code warnings in `threadlane-gpui`; no warning cleanup was included.

## Self-review / Concerns

- The runtime harness required one narrow forwarding API to expose the already-existing open-run compaction procedure to the session adapter. It introduces no dependency or alternate persistence path.
- Persistence remains append-only. A post-accept I/O error is refreshed from disk and returned as terminal; provider send is blocked.
- Manual telemetry cannot reconstruct historical pre-compaction usage after the legacy manual procedure has already committed, so its pre/post values use the canonical post-commit estimate while preserving truthful reason/model/budget/generation metadata.

## Comprehensive review fix wave

### Recovery inventory

The timed-out tree already contained partial fixes for accepted-run validation, durable steering/follow-up queueing, provider-boundary failure handling, overflow retry signaling, retained-tail occurrence appends, cancellation checks, and manual telemetry. Review against the Task 5 brief, `task-5-base`, the committed Task 5 package, and the actual diff found that these were directionally correct but not yet cleanly verified:

- **Already implemented and retained:** required boundary preparation before `ProviderRequestStarted`; provider send blocked on preparation/manifest persistence errors; one-shot runtime overflow retry; open foreground operation across checkpointing; append-by-occurrence retained-tail persistence; compact accepted-run token validation; steering/follow-up persistence before in-memory enqueue.
- **Still incomplete and finished here:** the ineffective strict-retry fixture never reached its first checkpoint; the duplicate fixture tested only a helper rather than a real checkpoint/tail commit; manual `retained_tail_tokens` was derived from total post-context and was not truthful; cancellation was checked only when compaction was required, permitting a below-trigger provider boundary after abort; cancellation-after-checkpoint durability was untested; effective tool-schema manifest assertions assumed one tool rather than proving the exact shortlisted schema; the runtime/session suites were not clean.
- **No findings remain open.**

### Correctness changes and stronger coverage

- Moved cancellation gating ahead of the below-trigger fast path. An accepted abort now blocks every later provider boundary. Once checkpoint acceptance begins, the gated procedure still runs to completion, preventing partial durable checkpoint state.
- Manual compaction now measures retained-tail tokens from the actual retained-tail messages and records exact pre-context, post-context, retained-tail, and removed-message values.
- Reworked the ineffective-compaction fixture so the normal pass commits generation 1, the strict pass executes exactly once and deterministically reports that no further history can be dropped, and no recursive third pass/commit occurs.
- Replaced helper-only duplicate coverage with a real prepared checkpoint commit containing two adjacent equal tail occurrences; both survive with distinct durable entry IDs and exact canonical multiplicity.
- Added cancellation-after-accepted-checkpoint/reload coverage proving the checkpoint is complete and stable across repeated opens, no later compaction/provider-start appears, and the foreground operation remains open.
- Strengthened effective-context capture coverage to compare the manifest's single tool-schema item against the exact shortlisted schema passed to the preparer/provider: dynamic tool count, SHA-256 digest, and token estimate. Ordering remains exactly `prepared -> started -> manifest -> sent`.
- Preserved provider-blocking coverage at both layers: a session compaction persistence failure returns terminally without a provider-start record, and runtime preparer/manifest persistence failures produce zero network sends.
- Preserved real overflow coverage: the fake provider emits an actual context-overflow error once, the preparer observes exactly `[false, true]`, and exactly two network calls occur. Session telemetry records exactly one `OverflowRecovery` generation.

### Adjudication: `run_accepted`, steering, and follow-up

These findings required fixes; they are not unrelated scope:

- The Task 5 architecture requires execution to remain attached to the already accepted durable foreground operation. Session history already validated the full `AcceptedRun`, but the public runtime `run_accepted(run_id, lane, accepted_through_seq)` accepted a smaller token and previously started provider work without proving that token against its refreshed store. The runtime now refreshes, verifies the accepted prefix, lane, and still-open operation, and rejects invented/stale tokens before provider activity. This is defense at the public execution boundary, not a second persistence path.
- The committed base showed `steer`/`follow_up` ignoring both enqueue and drive errors and then adding the message to volatile queues anyway. That contradicts the harness-first durability model used by Task 5 and could expose non-durable context to subsequent attempts. They now enter volatile queues only after gated persistence completes; failures emit terminally visible `AgentError` events.

### Fix-wave verification

Focused fixtures pass for adaptive ordering, reload projection, provider-blocking persistence failure, ineffective normal+strict retry, real one-shot overflow recovery, exact duplicate tail retention, truthful manual telemetry, cancellation before compaction, cancellation after accepted checkpoint, effective context capture, compact accepted-run validation, and provider-preparation failure.

Full verification:

- `cargo test -p threadlane-runtime` — PASS, 87 unit tests; 2 performance and 2 doc tests ignored.
- `cargo test -p threadlane-session` — PASS, 119 tests.
- `cargo check -p threadlane-gpui` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.

The pre-existing `threadlane-session::supervisor` unused-import warning and five GPUI dead-code warnings remain unrelated. The earlier manual-telemetry concern is superseded: pre, post, retained-tail, and compacted-message metrics are now all measured from their actual contexts.

## Final durability recovery (2026-03-18)

Closed the four final review findings without broadening scope:

1. **Append-only atomic checkpoint + tail + telemetry.** Compaction now stages the complete procedure and crosses the persistence gate once through `SessionStore::append_actions_atomically`. `JsonlStore` validates and sequences against a disposable reducer, emits one synced atomic-batch frame, and only then reloads observable state. The writer never truncates or rewrites canonical bytes. Atomic frames start behind a newline barrier, so a crash/disk failure that leaves a partial final frame is ignored on recovery and quarantined by the next append rather than concatenated with it. `torn_atomic_frame_is_quarantined_without_truncating_canonical_bytes` injects a synced partial frame, reopens, appends, proves the prior byte prefix is unchanged, and proves the later record reloads.
2. **Full accepted-run proof on adoption.** `CodingAgent::adopt_harness_run` now calls the harness's complete `validate_accepted_run` proof after freshness and before accepting open-operation/context evidence; lane, run id, accepted prefix, acceptance record, and current reduced state are no longer inferred from a compact subset.
3. **Terminal queue durability.** Steering and follow-up terminal paths clone their queued messages, persist first, and clear/extend only after success. The focused runtime fixture covers both queues and proves persistence failure retains the original queued item and does not inject it into volatile turn context.
4. **Real zero-send provider fixture.** `compaction_persistence_failure_sends_zero_fake_provider_requests` runs the actual runtime turn path with `RecordingProvider`, injects failure at the durable compaction boundary, and proves neither provider-start tracing nor `ProviderPort::stream_request` occurs. This replaces reliance on file-mode behavior as the zero-send proof.

Final verification:

- `cargo test -p threadlane-runtime` — PASS, 89 unit tests; 2 performance and 2 doc tests ignored.
- `cargo test -p threadlane-session` — PASS, 119 tests.
- `cargo check -p threadlane-gpui` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.

Known unrelated warnings remain unchanged: the session test target's unused `Duration` import and five GPUI dead-code warnings.

## Final blocker closure

- Atomic frames now use a reserved `#!threadlane-atomic-v1 ` envelope. Recovery recognizes every partial envelope cut; pre-envelope arbitrary EOF fragments (including `{` and `{"at`) are append-only quarantined with an explicit torn-EOF suffix. Completed unrelated malformed legacy lines remain errors. The exhaustive fixture tests every byte cut, appends a later durable record, reopens, and verifies the complete prior byte prefix is unchanged.
- Accepted-run validation now proves the durable `OperationStarted`, user prompt identity/lane/ancestry, and attempt-1 assistant reservation identity/run/lane all lie within `accepted_through_seq` and the current durable prefix. Compact accepted tokens execute the same proof, preventing assistant-proof bypass. Malformed assistant, cross-run, and forged-prefix fixtures pass.
- Transcript paging expands every visible entry and compaction marker in an atomic frame, preserving frame order while counting all messages and stopping only after the whole cursor-addressable line. A cross-page multi-visible frame fixture passes.

Final verification (2026-08-22):

- `cargo test -p threadlane-runtime` — PASS, 91 tests; 2 performance and 2 doc tests ignored.
- `cargo test -p threadlane-session` — PASS, 119 tests.
- `cargo check -p threadlane-gpui` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.

Unchanged warnings: one unused session test import and five GPUI dead-code warnings.

## Strict JSONL recovery-boundary closure

- `read_strict` now quarantines malformed atomic fragments only when they are the unterminated physical EOF tail or carry the reserved torn-EOF continuation marker and match the modern/legacy atomic framing prefix. Newline-terminated malformed lines that merely resemble a framing sentinel, suffix, or legacy prefix are rejected.
- Append-boundary sealing now validates the payload behind the modern sentinel and marks only recognizable malformed atomic fragments. This preserves append-only recovery and makes every modern frame cut explicit before a later append.
- Four negative fixtures cover completed partial-sentinel, full-sentinel junk, arbitrary torn-suffix, and legacy atomic-prefix lines. The exhaustive every-byte-cut fixture still proves later append/reopen recovery and preservation of every prior byte.

Verification:

- `cargo test -p threadlane-runtime harness::jsonl::tests:: -- --nocapture` — PASS, 23 tests.
- `cargo test -p threadlane-runtime` — PASS, 95 tests; 2 performance and 2 doc tests ignored.
- `cargo test -p threadlane-session` — PASS, 119 tests.
- `cargo check -p threadlane-gpui` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.

Warnings remain unchanged: one unused session test import and five GPUI dead-code warnings.

## Transcript paging parser parity closure

- Transcript paging now applies the same recoverable atomic-fragment predicate as strict JSONL loading. Only malformed physical-EOF atomic fragments or recovery-boundary fragments carrying the torn-EOF marker are skipped; valid unterminated JSON remains readable and unrelated malformed EOF data is rejected.
- Newline-terminated partial modern sentinels, full modern-sentinel junk, arbitrary torn-marker suffixes, and malformed legacy atomic prefixes now fail transcript paging just as they fail strict loading.
- Four negative paging fixtures cover those malformed shapes. The exhaustive every-byte atomic-frame cut fixture now also reads the recovered transcript at every cut (including legacy prefix cuts), while the multi-visible-item atomic page fixture remains intact.

Verification:

- `cargo test -p threadlane-runtime harness::jsonl::tests:: -- --nocapture` — PASS, 27 tests.
- `cargo test -p threadlane-runtime` — PASS, 99 unit tests; 2 performance and 2 doc tests ignored.
- `cargo test -p threadlane-session` — PASS, 119 tests.
- `cargo check -p threadlane-gpui` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `git diff --check` — PASS.

Warnings remain unchanged: one unused session test import and five GPUI dead-code warnings.
