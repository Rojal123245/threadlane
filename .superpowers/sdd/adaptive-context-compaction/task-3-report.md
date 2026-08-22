# Task 3 report

## Status

Implemented and committed the Task 3 runtime boundary API and ordering changes in commit `1b2c094` (`feat(runtime): prepare context before provider attempts`). The four prescribed runtime files are the only files in the commit.

The focused and runtime suites pass. The required GPUI check was run but is blocked by an existing downstream exhaustive match in `threadlane-session`; extending `ProviderTraceEvent::ContextManifest` as prescribed requires that consumer to add `..` or bind the four new fields. I did not edit or commit the non-prescribed session file.

## Implementation evidence

- Added the exact public `ProviderBoundaryRequest`, `ProviderBoundaryResult`, and asynchronous `ProviderBoundaryPreparer` API and re-exported it from `threadlane-runtime`.
- Added `AgentRuntime::set_provider_boundary_preparer`, defaulted the callback to `None`, and passed it into `TurnDriver`.
- Resolves the effective task/fallback model and computes the environment shortlist before provider-attempt preparation.
- Serializes the exact shortlisted `AgentToolDefinition` slice once and passes that schema to the callback and request estimator/manifest projection.
- Awaits preparation and installs the returned canonical messages before allocating a request ID, recording `ProviderTraceEvent::Started`, constructing/sending the request, or crossing the network boundary.
- Preparation failure emits `AgentError` with `context preparation failed:` and returns without starting a request.
- Context manifests now include model, optional context limit, estimate flag, and compaction generation. Token estimates are computed from the messages actually serialized into `RuntimeRequest` and the exact shortlisted schema.
- Provider overflow recovery remains single-shot. Durable runtimes with a preparer loop once with `overflow_recovery: true`; direct non-durable runtimes retain the existing in-memory compaction path.

## TDD evidence

### RED

Command:

```bash
cargo test -p threadlane-runtime preparation_finishes_before_provider_started_and_network_send -- --nocapture
```

Expected compile failure was observed before implementation:

```text
error[E0432]: unresolved imports `crate::provider::ProviderBoundaryRequest`, `crate::provider::ProviderBoundaryResult`
error[E0599]: no method named `set_provider_boundary_preparer` found for struct `runtime::AgentRuntime`
error: could not compile `threadlane-runtime` (lib test) due to 2 previous errors
```

The full RED output was recorded during implementation at `/tmp/task3-red.log`.

### GREEN

```text
cargo test -p threadlane-runtime preparation_finishes_before_provider_started_and_network_send -- --nocapture
# PASS: 1 passed; 0 failed

cargo test -p threadlane-runtime non_durable_runtime_keeps_direct_compaction -- --nocapture
# PASS: 1 passed; 0 failed

cargo test -p threadlane-runtime
# PASS: 78 passed; 0 failed; 2 ignored
# Doc tests: 0 failed; 2 ignored
```

The ordering test records and asserts exactly:

```text
prepared -> started -> sent
```

It also verifies that preparation receives the effective model and a non-empty, deserializable shortlisted tool-definition schema.

## Additional checks

```text
git diff --check
# PASS before commit

cargo check -p threadlane-gpui
# RUN, FAIL in threadlane-session due to downstream exhaustive ContextManifest match
```

GPUI diagnostic:

```text
error[E0027]: pattern does not mention fields `model`, `context_limit`,
`context_limit_is_estimate`, `compaction_generation`
  --> crates/threadlane-session/src/coding_agent/harness.rs:374:13
```

The minimal downstream compatibility change is to add `..` to that match (or consume the new telemetry fields). It was intentionally not made because Task 3 explicitly prescribes committing only:

- `crates/threadlane-runtime/src/provider.rs`
- `crates/threadlane-runtime/src/runtime.rs`
- `crates/threadlane-runtime/src/turn_driver.rs`
- `crates/threadlane-runtime/src/lib.rs`

## Self-review

- Confirmed the callback is awaited before request-ID allocation, `Started`, manifest recording, and `stream_request` spawning.
- Confirmed callback messages, rather than stale turn messages, are serialized into `RuntimeRequest` and manifest items.
- Confirmed direct compaction remains gated to runtimes without durable message recording.
- Confirmed only one provider overflow recovery can occur and durable callbacks receive the recovery flag on the second attempt.
- Confirmed no dependencies were added and no generated/deployed content was touched.
- Confirmed the commit contains exactly the four prescribed runtime files.

## Concern

Commit `1b2c094` alone does not compile the full GPUI dependency graph until the downstream session match is made forward-compatible with the additive `ContextManifest` fields. Runtime-focused checks are green; the parent task should make the one-line consumer adaptation in the appropriate session integration task.

## Review-fix follow-up

### Status

All Task 3 review findings are fixed. The earlier concern above is resolved: the downstream `threadlane-session` match now ignores additive `ContextManifest` fields, and `cargo check -p threadlane-gpui` completes successfully.

### RED

Added assertions before changing production behavior, then ran:

```text
cargo test -p threadlane-runtime preparation_finishes_before_provider_started_and_network_send -- --nocapture
```

The test failed at `runtime.rs` because the preparer's serialized schema contained `AgentToolDefinition` values while the captured `RuntimeRequest.tools` contained provider chat-completions tool values. This reproduced the reviewed schema/request mismatch.

### GREEN and implementation evidence

- `TurnDriver` now creates the shortlisted provider tool values exactly once, serializes that exact array for `ProviderBoundaryRequest.tool_schema_json`, and moves the same values into `RuntimeRequest.tools`.
- The ordering test captures both preparer schema and provider request tools and asserts exact JSON equality while preserving `prepared -> started -> sent`.
- Added `preparation_failure_starts_no_provider_activity`, proving a preparer error records neither `ProviderTraceEvent::Started` nor a provider send.
- Added `overflow_recovery_is_true_only_on_overflow_retry`, proving preparer observations are exactly `[false, true]` across one provider overflow and its sole retry, with exactly two network calls.
- Preserved the existing non-durable direct-compaction test and effective-model assertion.
- Added the minimal `..` to the exhaustive session `ContextManifest` match.

Focused tests:

```text
cargo test -p threadlane-runtime preparation_finishes_before_provider_started_and_network_send -- --nocapture  # PASS
cargo test -p threadlane-runtime preparation_failure_starts_no_provider_activity -- --nocapture                # PASS
cargo test -p threadlane-runtime overflow_recovery_is_true_only_on_overflow_retry -- --nocapture                # PASS
cargo test -p threadlane-runtime non_durable_runtime_keeps_direct_compaction -- --nocapture                     # PASS
```

Full verification:

```text
cargo test -p threadlane-runtime                                                                          # PASS: 80 passed; perf/doc tests ignored as declared
cargo check -p threadlane-gpui                                                                            # PASS (5 existing dead-code warnings)
rustfmt --edition 2021 --check crates/threadlane-runtime/src/runtime.rs crates/threadlane-runtime/src/turn_driver.rs crates/threadlane-session/src/coding_agent/harness.rs  # PASS
git diff --check                                                                                           # PASS
```
