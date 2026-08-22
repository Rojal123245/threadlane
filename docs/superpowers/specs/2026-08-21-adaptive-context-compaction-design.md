# Adaptive Context Compaction and Metering Design

## Summary

Make durable coding sessions prepare model context before every provider attempt, including attempts that follow tool execution inside one foreground run. When projected context reaches an adaptive model-aware budget, commit a durable compaction before starting the next provider request. Preserve the complete human-visible transcript, expose a compact timeline marker, and drive the context-window meter from the latest provider context manifest rather than cumulative session usage.

This design fixes two independent problems observed in `session_1787358568140675000`: compaction was never reconsidered during a 102-attempt tool loop, and the UI compared 11.7M cumulative processed tokens with a 1M single-request context limit.

## Evidence and Root Cause

The session journal contains 2,137 records and 152 completed provider requests. It contains no compaction operation or `UsageCause::Compaction` record. The eighth foreground run issued 102 provider attempts. Persisted context manifests grew from approximately 47.5k to 103.7k estimated tokens during that run.

Durable coding sessions currently check `auto_compact_history` before accepting a foreground prompt. `TurnDriver` deliberately skips its per-attempt compaction path when a durable message recorder is present because rewriting runtime state without first committing the harness branch would make recovery inconsistent. Consequently, a long tool loop can cross the threshold after its only durable compaction check.

The GPUI context meter currently uses the session's accumulated `TokenUsage.total_tokens`. In this session, provider records report approximately 266.6k uncached input tokens, 11.7M cache-read tokens, and 30.3k output tokens across all requests. That cumulative activity is useful as “Total processed,” but it is not the number of tokens in the latest request. The latest manifest's approximately 103.7k estimate is the correct available source for current context.

## Goals

- Re-evaluate durable model context before every provider attempt.
- Compact at a safe harness boundary before a provider request can observe rewritten context.
- Adapt the trigger to the effective task or fallback model while bounding repeated-input cost on very large models.
- Keep the full transcript visible and reloadable across compactions.
- Show a subtle durable `Context compacted · before → after` marker.
- Drive the meter from current model-facing context and keep cumulative provider usage separate.
- Make compaction, cancellation, persistence failure, and overflow recovery deterministic and testable.
- Preserve direct in-memory compaction for non-durable runtimes.

## Non-Goals

- Rewriting or retroactively compacting existing session journals.
- Deleting historical transcript entries or tool results from durable storage.
- Showing the internal compaction checkpoint summary as a normal chat message.
- Replacing provider token accounting with local estimates.
- Implementing semantic retrieval or a new long-term memory subsystem.
- Pixel-level redesign of the composer or hover card beyond the clarified metrics and marker.

## Chosen Approach

The durable coding-agent layer owns provider-boundary context preparation. `TurnDriver` invokes an optional preparation hook immediately before recording `ProviderRequestStarted`. Non-durable runtimes continue using their existing direct in-memory compaction path. Durable sessions use the hook to coordinate the runtime turn state and harness journal atomically from the provider's perspective.

The preparation hook receives the resolved effective model for that attempt. This matters when model roles select a task model different from the user-facing base model or when fallback routing changes the model within a run.

No provider request starts until context preparation succeeds. If compaction is required, the session commits the compaction operation, appends the retained tail, re-projects canonical model context into runtime state, and verifies the reduced estimate before returning control to `TurnDriver`.

## Shared Model Context Metadata

Context-window metadata must have one reusable source of truth available below GPUI. Move the context-limit resolver from the GPUI-only model catalog into a public `threadlane-runtime` model-metadata module; keep display labels, provider icons, credential filtering, and picker ordering in GPUI. `threadlane-session` and GPUI both already depend on `threadlane-runtime`, while `threadlane-provider` depends on runtime, so this location avoids a new dependency and a provider/runtime cycle. The durable context policy and GPUI meter consume the runtime resolver rather than maintaining parallel limit tables.

The resolver returns:

- effective model identifier,
- declared context limit,
- whether the limit is known or a fallback estimate.

Persist each context manifest with optional `effective_model`, `context_limit`, and `context_limit_is_estimate` fields. Optional fields preserve compatibility with old JSONL records. Existing manifests without those fields remain readable and use their associated `ProviderRequestStarted` model when possible; otherwise the UI may show usage without a denominator.

## Adaptive Budget Policy

The first implementation uses explicit defaults held in centralized agent configuration:

- Unknown-model fallback context limit: 128k tokens.
- Required headroom: the greater of 32k tokens or 20% of the model limit.
- Model-safe request budget: model limit minus required headroom.
- Repeated-input cost ceiling: 256k tokens.
- Compaction trigger: the lesser of the model-safe request budget and 256k tokens.
- Retained recent-tail target: the lesser of 64k tokens and 25% of the model limit, with a 20k-token floor when the model-safe budget permits it.

All arithmetic is saturating. Invalid limits smaller than the minimum headroom are treated as unknown and use the fallback. These defaults produce representative policies of:

- 128k model: compact near 96k and retain approximately 32k.
- 1M model: compact near 256k and retain approximately 64k.
- Unknown model: use the 128k fallback and mark the limit as estimated.

The 256k ceiling deliberately balances context quality with repeated cached-input cost on million-token models. It is not a claim that those models cannot accept more context. Configuration owns these values so future provider-specific measurements can tune them without changing durable semantics.

A compaction check itself is cheap and runs before every attempt. A new checkpoint is created only when the projected estimate reaches the trigger. Since successful compaction must reduce context below the trigger, no separate time-based cooldown is needed. The committed compaction generation and post-compaction estimate are nevertheless recorded for diagnostics and to reject duplicate commits during recovery.

## Provider-Boundary Data Flow

For every provider attempt:

1. Drain and durably persist pending steering messages as today.
2. Resolve the effective task or fallback model for this attempt.
3. Invoke provider-boundary context preparation before `ProviderRequestStarted`.
4. Project the current canonical model-facing lane context.
5. Estimate it with the same estimator used to construct `ContextManifestCaptured`.
6. Resolve model metadata and calculate the adaptive trigger and retained-tail target.
7. If below the trigger, return the projected messages unchanged.
8. If at or above the trigger:
   - construct the existing checkpoint summary over dropped model-facing history,
   - accept and drive the durable harness compaction operation to completion,
   - append the retained recent tail while preserving valid assistant-tool-call/tool-result boundaries,
   - re-project canonical lane context into the runtime turn,
   - calculate the post-compaction estimate,
   - persist compaction telemetry,
   - verify the estimate is below the request budget.
9. Record `ProviderRequestStarted`.
10. Persist `ContextManifestCaptured` with the effective model, context limit, estimated-limit flag, estimated current tokens, and compaction generation.
11. send the provider request.

The order is an invariant: a required compaction commit always precedes the request-start record and network call.

## Durable Compaction Telemetry

Keep the existing compaction operation, summary entry, usage record, and branch movement as the authority for model-context recovery. Add explicit durable telemetry for UI and diagnostics rather than inferring sizes from display history.

The telemetry captures:

- compaction generation,
- reason (`adaptive_budget`, `overflow_recovery`, or `manual`),
- effective model and resolved context limit,
- pre-compaction estimated tokens,
- post-compaction estimated tokens,
- retained-tail target and actual estimate,
- compacted message count,
- associated compaction run identifier.

The record is appended only after canonical context has been re-projected and measured. Recovery treats an existing committed generation as complete and does not append a duplicate marker.

## Transcript Semantics

Model-facing history and human-visible history are different projections of the same append-only journal:

- Compaction changes the preferred model-context branch to a checkpoint summary plus retained recent tail.
- Transcript paging continues to expose the original user, assistant, reasoning, and tool activity records.
- Internal `compaction_summary` content remains hidden from ordinary chat rendering.
- Durable compaction telemetry projects one compact system timeline marker at the correct boundary: `Context compacted · 742k → 118k`.
- Marker hover detail may show reason, model, limit, and retained-tail size.
- Reloading the JSONL reconstructs the same transcript and marker without transient UI state.

No old session receives a fabricated marker. `session_1787358568140675000` has no compaction record, so it continues to show none.

## Context Meter Semantics

The hover card separates three concepts.

### Current context

Use the newest `ContextManifestCaptured.total_estimated_tokens` for the active lane and latest effective request. This value drives the percentage, progress bar, and warning color. It is a request-context estimate, not cumulative usage.

Immediately after a committed compaction and before the next manifest is recorded, use the telemetry's post-compaction estimate provisionally. The next manifest replaces it.

### Context limit

Use the effective model and context limit persisted with the latest manifest. Do not compare historical context with the model currently highlighted in the picker. For legacy records, associate the manifest with its provider request and model where possible. If no reliable limit exists, show the token estimate with an “estimated limit” qualifier or omit the percentage rather than manufacturing precision.

After a model switch, retain the last completed request's labeled measurement until preparation for the new effective model begins; then show `Estimating…` until its first manifest arrives. Old usage is never reinterpreted against the new model.

### Total processed

Continue accumulating provider input, cache-read, cache-write, and output usage across the complete session. Display this separately as activity/cost telemetry. It never controls the context bar. Preserve cache-hit percentage as a separate cumulative statistic.

A representative card is:

```text
Context Window                  10%
103.7k / 1.0M

Total processed                 11.7M
Cache hit                         98%

Last compacted                  3m ago
Context is compacted automatically when needed.
```

For the reported legacy session, the expected result is approximately 103.7k current context, 11.7M total processed, and no “Last compacted” row.

## Failure and Cancellation Semantics

### Persistence failure

If accepting, driving, or recording a required compaction fails, do not record or send the provider request. Surface a durable harness/context-preparation error. Runtime state must be re-synchronized from the canonical lane before any later retry.

### Ineffective compaction

If the first post-compaction estimate still exceeds the model-safe request budget, perform one stricter compaction using half the original retained-tail target, subject to preserving a valid recent turn boundary. If that still does not fit, stop with a clear context-preparation error. Never continue recursively compacting.

### Provider overflow

Preserve overflow detection as defense in depth. A provider context-overflow response permits one emergency durable compaction with the stricter retained-tail target, followed by one provider retry. Persist the reason as `overflow_recovery`. Existing per-turn recovery guards prevent an unbounded retry loop.

### Cancellation

Do not interrupt an already accepted compaction midway through its gated effects. Drive it to a consistent terminal state, synchronize runtime context, and then honor cancellation before starting the provider request. Cancellation before compaction acceptance exits without changing context.

### Unknown or stale metadata

Use the conservative fallback policy and persist that the limit was estimated. A provider overflow can still invoke the bounded emergency path. Metadata correction affects future requests only and does not rewrite old manifests.

## Compatibility and Migration

- New JSONL fields are optional and serde-defaulted.
- New telemetry is additive; old stores remain readable.
- Legacy manifests can recover their effective model by request/run association where available.
- Sessions with no manifest fall back to the existing transcript-character estimate but show it as estimated and do not claim cumulative usage is current context.
- Non-durable agent consumers retain direct `TurnDriver` compaction.
- Manual compaction uses the same durable telemetry and transcript marker path.
- No migration rewrites existing session files.

## Testing Strategy

### Runtime compaction policy

- Calculate expected trigger and retained-tail targets for 128k, 1M, unknown, invalid, and very small model limits.
- Verify saturating arithmetic and explicit fallback labeling.
- Verify assistant tool calls remain paired with retained tool results.
- Verify successful compaction falls below the trigger.
- Verify an ineffective first compaction performs exactly one stricter attempt.

### Durable session integration

- Model a single foreground run with more than 100 provider/tool attempts and prove context preparation is invoked before every attempt.
- Grow context past the adaptive trigger during that run and verify compaction occurs before the next `ProviderRequestStarted`.
- Assert record ordering: compaction completion and telemetry precede request start and manifest.
- Reload the store and verify canonical model context is summary plus retained tail.
- Verify full transcript messages remain pageable after compaction.
- Verify persistence failure prevents the provider call.
- Verify cancellation leaves a terminal compaction operation and starts no provider request.
- Verify overflow recovery compacts and retries once only.
- Verify fallback-model attempts use fallback-model metadata.

### GPUI projection and rendering

- Project current context from the latest manifest rather than accumulated `TokenUsage`.
- Keep Total processed and cache-hit calculations cumulative.
- Reconstruct a compaction marker and last-compacted metadata after reload.
- Omit a marker for legacy sessions with no compaction telemetry.
- Use provisional post-compaction context until the next manifest arrives.
- Do not compare the last request with a newly selected but unused model.
- Verify legacy-manifest association and estimated-limit fallback.

### Regression fixture

Build a compact synthetic journal matching the reported session's important shape: one durable foreground run, 102 provider attempts, repeated tool results, cumulative cache usage far larger than a single context, and no user-prompt boundary during growth. Assert that cumulative processed usage can exceed the model limit while the current-context meter remains accurate, and that the revised runtime compacts before crossing its adaptive budget.

## Validation Commands

Run the narrowest relevant unit and integration tests introduced by the implementation, followed by:

```bash
cargo test -p threadlane-runtime
cargo test -p threadlane-session
cargo check -p threadlane-gpui
git diff --check
```

Run `cargo test --workspace` if the shared model metadata move or new durable record affects additional crates.

## Success Criteria

- A durable session cannot silently pass its adaptive context budget during a long tool loop.
- Every required compaction is committed before the next provider request starts.
- Provider-boundary preparation uses the effective task or fallback model.
- A 1M model defaults to a balanced 256k repeated-input ceiling rather than the old fixed 96k trigger or an unsafe near-limit trigger.
- The context meter represents the latest model-facing request estimate and visibly drops after compaction.
- Total processed remains cumulative and may legitimately exceed the model's context limit.
- The complete transcript and a subtle compaction marker survive reload.
- Compaction and overflow retries are bounded, durable, and observable.
- Existing JSONL sessions and non-durable runtimes remain compatible.

## Alternatives Rejected

### UI-only correction

Using the latest manifest would fix the misleading meter but leave long durable tool loops unable to compact. It addresses presentation, not the runtime failure.

### Direct durable compaction inside `TurnDriver`

`TurnDriver` does not own harness branch commits. Rewriting only its in-memory messages can diverge from crash recovery and reload. The session-owned preparation hook preserves the durability boundary.

### Fixed 96k threshold for every model

A global threshold discards useful context too early on large models and may be too high for smaller ones after output headroom. Model-aware limits plus a cost ceiling better represent both safety and expense.

### Compact only after provider overflow

Overflow-first handling adds failed requests and latency and depends on provider-specific errors. It remains a bounded emergency defense, not the primary policy.

### Drive the meter from cumulative usage

Cumulative usage answers how much work the session processed. It cannot describe a single request context and routinely exceeds the model limit through repeated cached prefixes.

### Hide or delete compacted transcript history

Durable model context can be reduced without removing user-visible history. Deletion would harm auditability and violate the requirement to preserve the complete transcript.
