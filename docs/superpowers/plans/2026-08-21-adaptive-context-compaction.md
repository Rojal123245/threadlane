# Adaptive Context Compaction and Metering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compact durable coding-session context safely between provider attempts and display current request context separately from cumulative processed usage.

**Architecture:** `TurnDriver` gains an optional asynchronous provider-boundary preparer. Durable `CodingAgent` runs that hook through its shared `CodingSessionHarness`, commits any required compaction before `ProviderRequestStarted`, and returns the canonical model-facing messages to runtime. Additive harness telemetry records the compaction and request metadata; GPUI projects the newest manifest/compaction telemetry into a truthful context meter while paged transcript projection emits a durable marker without adding it to model context.

**Tech Stack:** Rust 2021, Tokio, serde/serde_json, Threadlane harness JSONL V2, GPUI/gpui-component, existing `AgentConfig`, `TurnDriver`, `CodingSessionHarness`, `JsonlStore`, and `AppState` projections.

## Global Constraints

- Keep the complete human-visible transcript; compaction changes only canonical model context.
- A required durable compaction must finish before `ProviderRequestStarted` and before the network request.
- Use the effective task or fallback model for every attempt, not merely the picker selection.
- Default unknown-model limit: 128,000 tokens; headroom: `max(32,000, 20%)`; repeated-input ceiling: 256,000 tokens.
- Retained-tail target: `min(64,000, 25%)`, with a 20,000-token floor when the safe budget permits it.
- Allow one stricter compaction attempt and one provider-overflow recovery; never retry recursively.
- Keep new JSONL fields optional/defaulted and new records additive; never rewrite existing session files.
- Reuse the existing model catalog, compaction helpers, harness procedures, transcript pager, and projections; add no dependency or parallel state path.
- Keep non-durable direct `TurnDriver` compaction behavior intact.
- Do not edit generated `target/` or deployed `.threadlane/` content.
- Rust/GPUI completion requires `cargo check -p threadlane-gpui` and `git diff --check`.

## File Structure

- Create `crates/threadlane-runtime/src/model_metadata.rs`: shared context-limit lookup and adaptive budget calculation only.
- Modify `crates/threadlane-runtime/src/lib.rs`: export model metadata and provider-boundary types.
- Modify `crates/threadlane-runtime/src/config.rs`: centralized adaptive-policy values and builder setters.
- Modify `crates/threadlane-runtime/src/compaction.rs`: public exact request estimator and adaptive retained-tail compaction result.
- Modify `crates/threadlane-runtime/src/provider.rs`: provider-boundary callback request/result types and enriched manifest event.
- Modify `crates/threadlane-runtime/src/runtime.rs`: store/configure the callback and pass it to `TurnDriver`.
- Modify `crates/threadlane-runtime/src/turn_driver.rs`: invoke preparation before request start, replace messages from the result, enrich manifest, and bound overflow recovery.
- Modify `crates/threadlane-runtime/src/harness/types.rs`: additive `ContextCompacted` record plus optional manifest metadata.
- Modify `crates/threadlane-runtime/src/harness/procedure.rs`: parameterize compaction reason while preserving manual compaction.
- Modify `crates/threadlane-runtime/src/harness/reducer.rs`, `trajectory.rs`, and `jsonl.rs`: accept/project/round-trip telemetry and page transcript markers.
- Modify `crates/threadlane-runtime/src/harness/mod.rs`: export new transcript and telemetry types.
- Modify `crates/threadlane-session/src/coding_agent/harness.rs`: persist enriched traces and expose an atomic durable preparation operation.
- Modify `crates/threadlane-session/src/coding_agent/durable.rs`: install/uninstall the preparation callback with existing run recorders.
- Modify `crates/threadlane-session/src/coding_agent/runtime.rs`: remove the prompt-only auto-compaction path after provider-boundary preparation is active.
- Modify `crates/threadlane-gpui/src/model_catalog.rs`: delegate context limits to runtime metadata while retaining picker/display responsibilities.
- Modify `crates/threadlane-gpui/src/state/app_state.rs`: project `ContextWindowInfo`, cumulative metrics, and transcript markers.
- Modify `crates/threadlane-gpui/src/screens/chat/view.rs`: render current context, effective model limit, cumulative totals, cache hit, and last-compacted state.
- Modify `AGENTS.md`: record the durable provider-boundary compaction invariant discovered by this work.

---

### Task 1: Shared Model Metadata and Adaptive Policy

**Files:**
- Create: `crates/threadlane-runtime/src/model_metadata.rs`
- Modify: `crates/threadlane-runtime/src/lib.rs`
- Modify: `crates/threadlane-runtime/src/config.rs`
- Modify: `crates/threadlane-gpui/src/model_catalog.rs`
- Test: inline `#[cfg(test)]` modules in `model_metadata.rs` and `model_catalog.rs`

**Interfaces:**
- Produces: `pub fn model_context_limit(model: &str) -> Option<usize>`.
- Produces: `pub struct ContextBudget { pub limit: usize, pub limit_is_estimate: bool, pub trigger_tokens: usize, pub retained_tail_tokens: usize, pub strict_retained_tail_tokens: usize }`.
- Produces: `pub fn context_budget(model: &str, config: &AgentConfig) -> ContextBudget`.
- Consumes later: Tasks 2, 4, and 5 use these exact names and fields.

- [ ] **Step 1: Write failing policy and catalog-delegation tests**

Add to `crates/threadlane-runtime/src/model_metadata.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;

    #[test]
    fn adaptive_budget_balances_known_large_and_unknown_models() {
        let config = AgentConfig::default();
        let large = context_budget("antigravity/gemini-3.7-flash", &config);
        assert_eq!(large.limit, 1_000_000);
        assert!(!large.limit_is_estimate);
        assert_eq!(large.trigger_tokens, 256_000);
        assert_eq!(large.retained_tail_tokens, 64_000);
        assert_eq!(large.strict_retained_tail_tokens, 32_000);

        let unknown = context_budget("unknown/model", &config);
        assert_eq!(unknown.limit, 128_000);
        assert!(unknown.limit_is_estimate);
        assert_eq!(unknown.trigger_tokens, 96_000);
        assert_eq!(unknown.retained_tail_tokens, 32_000);
        assert_eq!(unknown.strict_retained_tail_tokens, 20_000);
    }

    #[test]
    fn invalid_known_limit_uses_saturating_fallback_policy() {
        let mut config = AgentConfig::default();
        config.unknown_model_context_limit = 16_000;
        let budget = ContextBudget::from_limit(None, &config);
        assert_eq!(budget.limit, 128_000);
        assert_eq!(budget.trigger_tokens, 96_000);
    }
}
```

Update the existing `context_window_and_token_formatting` test in `crates/threadlane-gpui/src/model_catalog.rs` to assert:

```rust
assert_eq!(model_context_window("antigravity/gemini-3.7-flash"), 1_000_000);
assert_eq!(
    model_context_window("unknown/model"),
    threadlane_runtime::model_metadata::UNKNOWN_MODEL_CONTEXT_LIMIT as u32,
);
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
cargo test -p threadlane-runtime model_metadata::tests -- --nocapture
cargo test -p threadlane-gpui model_catalog::tests::context_window_and_token_formatting -- --nocapture
```

Expected: runtime fails because `model_metadata` and adaptive config fields do not exist; GPUI fails because it still owns the fallback table.

- [ ] **Step 3: Implement shared metadata and adaptive configuration**

Create `crates/threadlane-runtime/src/model_metadata.rs` with the existing context limits moved verbatim from `model_catalog.rs`, then add:

```rust
use crate::AgentConfig;

pub const UNKNOWN_MODEL_CONTEXT_LIMIT: usize = 128_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextBudget {
    pub limit: usize,
    pub limit_is_estimate: bool,
    pub trigger_tokens: usize,
    pub retained_tail_tokens: usize,
    pub strict_retained_tail_tokens: usize,
}

impl ContextBudget {
    pub fn from_limit(limit: Option<usize>, config: &AgentConfig) -> Self {
        let minimum_valid = config.context_minimum_headroom_tokens.saturating_mul(2);
        let known = limit.filter(|value| *value >= minimum_valid);
        let fallback = config
            .unknown_model_context_limit
            .max(UNKNOWN_MODEL_CONTEXT_LIMIT);
        let limit = known.unwrap_or(fallback);
        let proportional_headroom = limit
            .saturating_mul(config.context_headroom_percent)
            .div_ceil(100);
        let headroom = proportional_headroom.max(config.context_minimum_headroom_tokens);
        let safe_budget = limit.saturating_sub(headroom);
        let trigger_tokens = safe_budget.min(config.context_repeated_input_ceiling_tokens);
        let proportional_tail = limit
            .saturating_mul(config.context_retained_tail_percent)
            .div_ceil(100);
        let mut retained_tail_tokens = proportional_tail
            .min(config.context_maximum_retained_tail_tokens)
            .min(trigger_tokens.saturating_sub(1));
        if trigger_tokens > config.context_minimum_retained_tail_tokens {
            retained_tail_tokens = retained_tail_tokens
                .max(config.context_minimum_retained_tail_tokens);
        }
        let strict_retained_tail_tokens = retained_tail_tokens
            .div_ceil(2)
            .max(config.context_minimum_retained_tail_tokens)
            .min(trigger_tokens.saturating_sub(1));
        Self {
            limit,
            limit_is_estimate: known.is_none(),
            trigger_tokens,
            retained_tail_tokens,
            strict_retained_tail_tokens,
        }
    }
}

pub fn context_budget(model: &str, config: &AgentConfig) -> ContextBudget {
    ContextBudget::from_limit(model_context_limit(model), config)
}
```

Add these `pub(crate)` fields and defaults to `AgentConfig` in `config.rs`:

```rust
pub(crate) unknown_model_context_limit: usize,             // 128_000
pub(crate) context_minimum_headroom_tokens: usize,         // 32_000
pub(crate) context_headroom_percent: usize,                // 20
pub(crate) context_repeated_input_ceiling_tokens: usize,   // 256_000
pub(crate) context_minimum_retained_tail_tokens: usize,    // 20_000
pub(crate) context_maximum_retained_tail_tokens: usize,    // 64_000
pub(crate) context_retained_tail_percent: usize,           // 25
```

Expose matching builder methods. Export `pub mod model_metadata;` from `lib.rs`. Replace GPUI's context-limit match with:

```rust
pub(crate) fn model_context_window(model: &str) -> u32 {
    threadlane_runtime::model_metadata::model_context_limit(model)
        .unwrap_or(threadlane_runtime::model_metadata::UNKNOWN_MODEL_CONTEXT_LIMIT)
        .min(u32::MAX as usize) as u32
}
```

Keep provider labels, icons, ordering, and credential filtering in GPUI.

- [ ] **Step 4: Run focused tests and checks**

Run the two Step 2 commands again, then:

```bash
cargo check -p threadlane-runtime
cargo check -p threadlane-gpui
```

Expected: all commands pass; no duplicate context-limit table remains in GPUI.

- [ ] **Step 5: Commit**

```bash
git add crates/threadlane-runtime/src/model_metadata.rs crates/threadlane-runtime/src/lib.rs crates/threadlane-runtime/src/config.rs crates/threadlane-gpui/src/model_catalog.rs
git commit -m "feat(runtime): add adaptive context budgets"
```

---

### Task 2: Exact Context Estimation and Boundary-Safe Compaction

**Files:**
- Modify: `crates/threadlane-runtime/src/compaction.rs`
- Test: inline tests in `crates/threadlane-runtime/src/compaction.rs`

**Interfaces:**
- Consumes: `ContextBudget` from Task 1.
- Produces: `pub struct PreparedCompaction { pub messages: Vec<AgentMessage>, pub pre_tokens: usize, pub post_tokens: usize, pub compacted_messages: usize, pub retained_tail_tokens: usize }`.
- Produces: `pub fn estimate_request_tokens(messages: &[AgentMessage], tool_schema_json: Option<&str>, config: &AgentConfig) -> usize`.
- Produces: `pub fn compact_for_budget(messages: &[AgentMessage], tool_schema_json: Option<&str>, retained_tail_tokens: usize, config: &AgentConfig) -> Option<PreparedCompaction>`.

- [ ] **Step 1: Add failing estimator and tool-boundary tests**

Add tests that use one system message, one assistant tool call, its tool result, and a final user message:

```rust
#[test]
fn request_estimator_includes_tool_schema_and_images() {
    let config = AgentConfig::default();
    let messages = vec![AgentMessage::UserWithImages {
        content: "x".repeat(400),
        images: vec![ImageAttachment {
            display_name: "image.png".into(),
            data_url: "data:image/png;base64,AA==".into(),
        }],
    }];
    assert_eq!(estimate_request_tokens(&messages, Some(&"t".repeat(400)), &config), 1_400);
}

#[test]
fn budget_compaction_retains_complete_tool_exchange() {
    let messages = tool_exchange_fixture(12_000);
    let result = compact_for_budget(&messages, None, 1_000, &AgentConfig::default()).unwrap();
    assert!(compaction_summary_text(&result.messages[1]).is_some());
    assert_valid_tool_pairs(&result.messages);
    assert!(result.post_tokens < result.pre_tokens);
    assert!(result.compacted_messages > 0);
}
```

Use existing `ToolCall`, `AgentMessage::Assistant`, and `AgentMessage::Tool` constructors in this test module; do not invent a second message representation.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p threadlane-runtime compaction::tests::request_estimator_includes_tool_schema_and_images -- --nocapture
cargo test -p threadlane-runtime compaction::tests::budget_compaction_retains_complete_tool_exchange -- --nocapture
```

Expected: FAIL because the two public helpers and result type do not exist.

- [ ] **Step 3: Implement the helpers by refactoring existing code**

Make `estimate_message_tokens` reusable rather than duplicating the quarter-character estimator. Implement:

```rust
#[derive(Debug, Clone)]
pub struct PreparedCompaction {
    pub messages: Vec<AgentMessage>,
    pub pre_tokens: usize,
    pub post_tokens: usize,
    pub compacted_messages: usize,
    pub retained_tail_tokens: usize,
}

pub fn estimate_request_tokens(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    config: &AgentConfig,
) -> usize {
    estimate_context_tokens(messages, config).saturating_add(
        tool_schema_json.map_or(0, |tools| tools.len().div_ceil(4)),
    )
}

pub fn compact_for_budget(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    retained_tail_tokens: usize,
    config: &AgentConfig,
) -> Option<PreparedCompaction> {
    let pre_tokens = estimate_request_tokens(messages, tool_schema_json, config);
    let compacted = compact_messages_to_token_budget(messages, retained_tail_tokens);
    if compacted.len() == messages.len() {
        return None;
    }
    let post_tokens = estimate_request_tokens(&compacted, tool_schema_json, config);
    let compacted_messages = messages.len().saturating_sub(compacted.len().saturating_sub(1));
    Some(PreparedCompaction {
        messages: compacted,
        pre_tokens,
        post_tokens,
        compacted_messages,
        retained_tail_tokens,
    })
}
```

Adjust `compact_messages_to_token_budget` so the chosen start index never retains a `Tool` message without its preceding assistant tool call and never retains an assistant tool call without all immediately following results. Reuse the existing pairing checks/tests around `token_compaction_keeps_tool_call_before_tool_result`.

- [ ] **Step 4: Run all compaction tests**

```bash
cargo test -p threadlane-runtime compaction::tests -- --nocapture
```

Expected: PASS, including all existing compaction tests.

- [ ] **Step 5: Commit**

```bash
git add crates/threadlane-runtime/src/compaction.rs
git commit -m "feat(runtime): prepare context to adaptive budgets"
```

---

### Task 3: Provider-Boundary Preparation Hook and Request Ordering

**Files:**
- Modify: `crates/threadlane-runtime/src/provider.rs`
- Modify: `crates/threadlane-runtime/src/runtime.rs`
- Modify: `crates/threadlane-runtime/src/turn_driver.rs`
- Modify: `crates/threadlane-runtime/src/lib.rs`
- Test: inline tests in `crates/threadlane-runtime/src/runtime.rs` and `turn_driver.rs`

**Interfaces:**
- Consumes: Task 1 model budget and Task 2 estimator.
- Produces: `ProviderBoundaryRequest`, `ProviderBoundaryResult`, and `ProviderBoundaryPreparer` exactly as below.
- Produces: `AgentRuntime::set_provider_boundary_preparer(Option<ProviderBoundaryPreparer>)`.
- Consumed by: Task 5 installs the durable implementation.

- [ ] **Step 1: Write failing request-order and non-durable-regression tests**

Add a recording fake provider and preparer. The core assertion must be:

```rust
#[tokio::test]
async fn preparation_finishes_before_provider_started_and_network_send() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let preparer_order = order.clone();
    runtime.set_provider_boundary_preparer(Some(Arc::new(move |request| {
        let order = preparer_order.clone();
        Box::pin(async move {
            order.lock().unwrap().push("prepared");
            Ok(ProviderBoundaryResult {
                messages: request.messages,
                context_limit: 128_000,
                context_limit_is_estimate: true,
                compaction_generation: 0,
                provisional_estimated_tokens: None,
            })
        })
    })));
    runtime.run("test").await;
    assert_eq!(&*order.lock().unwrap(), &["prepared", "started", "sent"]);
}
```

Add a second test with no preparer that crosses `auto_compaction_threshold_tokens` and asserts the existing non-durable direct compaction still reduces messages.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p threadlane-runtime preparation_finishes_before_provider_started_and_network_send -- --nocapture
cargo test -p threadlane-runtime non_durable_runtime_keeps_direct_compaction -- --nocapture
```

Expected: FAIL because boundary types/setter do not exist and current request-start ordering precedes preparation.

- [ ] **Step 3: Add callback types and runtime wiring**

In `provider.rs` add:

```rust
#[derive(Clone, Debug)]
pub struct ProviderBoundaryRequest {
    pub attempt: u32,
    pub model: String,
    pub messages: Vec<AgentMessage>,
    pub tool_schema_json: Option<String>,
    pub overflow_recovery: bool,
}

#[derive(Clone, Debug)]
pub struct ProviderBoundaryResult {
    pub messages: Vec<AgentMessage>,
    pub context_limit: usize,
    pub context_limit_is_estimate: bool,
    pub compaction_generation: u64,
    pub provisional_estimated_tokens: Option<usize>,
}

pub type ProviderBoundaryPreparer = Arc<
    dyn Fn(ProviderBoundaryRequest)
        -> Pin<Box<dyn Future<Output = Result<ProviderBoundaryResult, String>> + Send>>
        + Send
        + Sync,
>;
```

Add `provider_boundary_preparer: Option<ProviderBoundaryPreparer>` to `AgentRuntime`, initialize it to `None`, add the setter, and pass it into `TurnDriver`.

- [ ] **Step 4: Invoke the hook at the authoritative boundary**

In `TurnDriver::run_turns`, resolve the effective model and shortlist tools first. Serialize the exact shortlisted tool definitions once. Before creating a request ID or recording `ProviderTraceEvent::Started`, call:

```rust
let tool_schema_json = (!tool_definitions.is_empty())
    .then(|| serde_json::to_string(&tool_definitions).unwrap_or_default());
if let Some(preparer) = &self.provider_boundary_preparer {
    let messages = self.turn.lock().await.messages.clone();
    let prepared = preparer(ProviderBoundaryRequest {
        attempt: turn_number as u32,
        model: model.clone(),
        messages,
        tool_schema_json: tool_schema_json.clone(),
        overflow_recovery: overflow_recovery_attempted,
    })
    .await
    .map_err(|error| format!("context preparation failed: {error}"));
    match prepared {
        Ok(prepared) => {
            self.turn.lock().await.messages = prepared.messages;
            boundary_result = Some(prepared);
        }
        Err(error) => {
            self.emit_event(AgentEvent::AgentError { error });
            return;
        }
    }
}
```

Then allocate the request ID, record `Started`, build `RuntimeRequest`, build the manifest, record it, and call `stream_request`. Extend `ProviderTraceEvent::ContextManifest` with:

```rust
model: String,
context_limit: Option<usize>,
context_limit_is_estimate: bool,
compaction_generation: u64,
```

Use `estimate_request_tokens`/manifest items over the messages actually placed in `RuntimeRequest`. Keep `overflow_recovery_attempted` as the single retry guard; on overflow, loop once with `overflow_recovery: true` so durable preparation performs the strict checkpoint.

- [ ] **Step 5: Run focused and runtime tests**

```bash
cargo test -p threadlane-runtime preparation_finishes_before_provider_started_and_network_send -- --nocapture
cargo test -p threadlane-runtime non_durable_runtime_keeps_direct_compaction -- --nocapture
cargo test -p threadlane-runtime
```

Expected: PASS. The order assertion proves no provider trace/network boundary precedes preparation.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-runtime/src/provider.rs crates/threadlane-runtime/src/runtime.rs crates/threadlane-runtime/src/turn_driver.rs crates/threadlane-runtime/src/lib.rs
git commit -m "feat(runtime): prepare context before provider attempts"
```

---

### Task 4: Additive Harness Telemetry and Transcript Marker Paging

**Files:**
- Modify: `crates/threadlane-runtime/src/harness/types.rs`
- Modify: `crates/threadlane-runtime/src/harness/procedure.rs`
- Modify: `crates/threadlane-runtime/src/harness/reducer.rs`
- Modify: `crates/threadlane-runtime/src/harness/trajectory.rs`
- Modify: `crates/threadlane-runtime/src/harness/jsonl.rs`
- Modify: `crates/threadlane-runtime/src/harness/mod.rs`
- Test: inline tests in `types.rs`, `procedure.rs`, `trajectory.rs`, and `jsonl.rs`

**Interfaces:**
- Produces: `CompactionReason::{Manual, AdaptiveBudget, OverflowRecovery}`.
- Produces: additive `Record::ContextCompacted` and optional fields on `Record::ContextManifestCaptured`.
- Produces: `TranscriptItem::{Message(AgentMessage), ContextCompacted(ContextCompactedMarker)}` and `TranscriptPage.items`.
- Consumed by: Tasks 5 and 6.

- [ ] **Step 1: Write failing JSONL compatibility and paging tests**

Add three tests:

```rust
#[test]
fn legacy_context_manifest_deserializes_without_new_metadata() {
    let record: Record = serde_json::from_str(LEGACY_MANIFEST_JSON).unwrap();
    let Record::ContextManifestCaptured {
        effective_model,
        context_limit,
        context_limit_is_estimate,
        compaction_generation,
        ..
    } = record else { panic!("expected manifest") };
    assert_eq!(effective_model, None);
    assert_eq!(context_limit, None);
    assert!(!context_limit_is_estimate);
    assert_eq!(compaction_generation, 0);
}

#[test]
fn compaction_telemetry_round_trips() {
    let record = context_compacted_fixture(42, 742_000, 118_000);
    assert_eq!(serde_json::from_str::<Record>(&serde_json::to_string(&record).unwrap()).unwrap(), record);
}

#[test]
fn transcript_page_orders_compaction_marker_without_exposing_summary() {
    let path = write_transcript_with_compaction();
    let page = read_transcript_page(&path, None, 20).unwrap();
    assert!(matches!(page.items[1], TranscriptItem::ContextCompacted(_)));
    assert!(!page.items.iter().any(|item| matches!(item,
        TranscriptItem::Message(AgentMessage::Custom { custom_type, .. }) if custom_type == "compaction_summary"
    )));
}
```

The fixture must include original entries, a replacement `compaction_summary`, a `ContextCompacted` record, and a retained-tail entry.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p threadlane-runtime legacy_context_manifest_deserializes_without_new_metadata -- --nocapture
cargo test -p threadlane-runtime compaction_telemetry_round_trips -- --nocapture
cargo test -p threadlane-runtime transcript_page_orders_compaction_marker_without_exposing_summary -- --nocapture
```

Expected: FAIL because the new fields, record, and transcript item type do not exist.

- [ ] **Step 3: Add schema-compatible records**

In `harness/types.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    Manual,
    AdaptiveBudget,
    OverflowRecovery,
}

Record::ContextCompacted {
    id: String,
    seq: u64,
    lane: String,
    timestamp: u64,
    run_id: String,
    generation: u64,
    reason: CompactionReason,
    effective_model: TraceString,
    context_limit: usize,
    context_limit_is_estimate: bool,
    pre_tokens: usize,
    post_tokens: usize,
    retained_tail_target: usize,
    retained_tail_tokens: usize,
    compacted_messages: usize,
}
```

Add these fields to `ContextManifestCaptured`, each with `#[serde(default)]` and `skip_serializing_if` where applicable:

```rust
effective_model: Option<TraceString>,
context_limit: Option<usize>,
context_limit_is_estimate: bool,
compaction_generation: u64,
```

Update every exhaustive `Record` match (`id`, `seq`, `lane`, `run_id`, `attempt`, `with_seq`, reducer validation, trajectory) so `ContextCompacted` is accepted as observational telemetry. Do not let it alter lane reduction; the compaction procedure remains authoritative.

- [ ] **Step 4: Add an in-run compaction checkpoint procedure**

Keep `CompactionProcedure::accept` and manual idle-lane compaction unchanged. Add a separate method for durable provider boundaries because the main foreground operation is intentionally still open and nested `OperationStarted` records are invalid:

```rust
pub fn checkpoint_open_run<S: SessionStore>(
    store: &S,
    lane_name: &str,
    run_id: &str,
    summary: &str,
    reason: CompactionReason,
    effects: &mut GatedEffects,
) -> Result<(), ProcedureError> {
    let lane = Reducer::reduce(store)
        .lanes
        .get(lane_name)
        .cloned()
        .ok_or_else(|| ProcedureError::Invalid(format!("lane {lane_name} is missing")))?;
    if lane.open_operation.as_deref() != Some(run_id) {
        return Err(ProcedureError::Invalid(format!(
            "operation {run_id} is not open on lane {lane_name}"
        )));
    }
    let first_seq = next_seq_with_effects(store, effects);
    let source_leaf_id = lane.leaf_id;
    let summary_id = format!("compaction-{run_id}-{first_seq}-summary");
    effects.park(EffectAction::AppendEntry {
        id: format!("compaction-entry-action-{run_id}-{first_seq}"),
        entry: Entry {
            id: summary_id.clone(),
            parent_id: None,
            lane: lane_name.into(),
            seq: first_seq,
            timestamp: first_seq,
            message: AgentMessage::Custom {
                custom_type: "compaction_summary".into(),
                payload: serde_json::json!({
                    "schema_version": 1,
                    "summary": summary,
                    "checkpoint_kind": reason.as_str(),
                    "source_leaf_id": source_leaf_id,
                }),
            },
            surface_op: SurfaceOperation::Replace {
                start_seq: 1,
                end_seq: first_seq.saturating_sub(1),
                source_event_seqs: Vec::new(),
            },
            terminate: false,
        },
    })?;
    effects.park(EffectAction::AppendRecord {
        id: format!("compaction-move-action-{run_id}-{first_seq}"),
        record: Record::LaneMoved {
            id: format!("compaction-move-{run_id}-{first_seq}"),
            seq: first_seq + 1,
            lane: lane_name.into(),
            timestamp: first_seq + 1,
            run_id: run_id.into(),
            target_leaf_id: summary_id,
        },
    })?;
    Ok(())
}
```

Add `CompactionReason::as_str() -> &'static str`. Test that `checkpoint_open_run` rejects an idle/wrong run, moves the model-context leaf to the summary, and leaves `lane.open_operation == Some(run_id)` so tool-loop execution can continue. The session appends `ContextCompacted` only after driving these effects, restoring the retained tail, and measuring the canonical projection.

- [ ] **Step 5: Page messages and markers as one ordered stream**

In `jsonl.rs` add:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ContextCompactedMarker {
    pub seq: u64,
    pub timestamp: u64,
    pub pre_tokens: usize,
    pub post_tokens: usize,
    pub reason: CompactionReason,
    pub effective_model: String,
    pub context_limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    Message(AgentMessage),
    ContextCompacted(ContextCompactedMarker),
}

pub struct TranscriptPage {
    pub items: Vec<TranscriptItem>,
    pub next_cursor: Option<TranscriptCursor>,
    pub has_older: bool,
}
```

Replace `transcript_message` with `transcript_item`; return main-lane entries except internal `compaction_summary`, and return main-lane `ContextCompacted` records as markers. Continue counting only `Message` items toward `minimum_messages`, but retain encountered markers in byte/sequence order. Export the types in `harness/mod.rs`.

- [ ] **Step 6: Run harness tests**

```bash
cargo test -p threadlane-runtime harness::jsonl::tests -- --nocapture
cargo test -p threadlane-runtime harness::procedure::tests -- --nocapture
cargo test -p threadlane-runtime harness::trajectory::tests -- --nocapture
cargo test -p threadlane-runtime
```

Expected: PASS, including old JSONL fixtures and new marker ordering.

- [ ] **Step 7: Commit**

```bash
git add crates/threadlane-runtime/src/harness
git commit -m "feat(harness): record context compaction telemetry"
```

---

### Task 5: Durable Session Provider-Boundary Compaction

**Files:**
- Modify: `crates/threadlane-session/src/coding_agent/harness.rs`
- Modify: `crates/threadlane-session/src/coding_agent/durable.rs`
- Modify: `crates/threadlane-session/src/coding_agent/runtime.rs`
- Test: inline tests in those files

**Interfaces:**
- Consumes: `ProviderBoundaryPreparer`, `ProviderBoundaryRequest`, `ProviderBoundaryResult`, `ContextBudget`, `PreparedCompaction`, and `Record::ContextCompacted`.
- Produces: `CodingSessionHarness::prepare_provider_boundary(run_id: &str, request: ProviderBoundaryRequest, config: &AgentConfig) -> Result<ProviderBoundaryResult, String>`.
- Preserves: existing `install_run_trace_recorders` lifecycle and canonical `model_context("main")` recovery.

- [ ] **Step 1: Add failing durable ordering, reload, and long-loop tests**

Build tests around a temporary `CodingSessionHarness` and fake provider. Required assertions:

```rust
#[tokio::test]
async fn adaptive_compaction_commits_before_next_provider_attempt() {
    let (mut agent, path, provider) = long_tool_loop_fixture(102).await;
    agent.prompt("continue").await.unwrap();
    let store = JsonlStore::open_read_only(&path).unwrap();
    let records = store.records();
    let compacted_seq = records.iter().find_map(|record| match record {
        Record::ContextCompacted { seq, reason: CompactionReason::AdaptiveBudget, .. } => Some(*seq),
        _ => None,
    }).expect("adaptive compaction");
    let next_start_seq = records.iter().find_map(|record| match record {
        Record::ProviderRequestStarted { seq, .. } if *seq > compacted_seq => Some(*seq),
        _ => None,
    }).expect("request after compaction");
    assert!(compacted_seq < next_start_seq);
    assert_eq!(provider.attempts(), 102);
}

#[tokio::test]
async fn reload_uses_checkpoint_tail_but_transcript_keeps_original_entries() {
    let path = compacted_session_fixture().await;
    let store = JsonlStore::open_read_only(&path).unwrap();
    assert!(store.model_context("main").unwrap().checkpoint.is_some());
    assert!(store.transcript("main").entries.len() > store.model_context("main").unwrap().entries.len());
}
```

Add failure tests where `append_record_gated` is forced to fail and assert the fake provider's send count remains zero. Add an ineffective-compaction fixture and assert exactly two compaction attempts (normal then strict), then a terminal `AgentError`. Add overflow fixture and assert exactly one emergency compaction/provider retry.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p threadlane-session adaptive_compaction_commits_before_next_provider_attempt -- --nocapture
cargo test -p threadlane-session reload_uses_checkpoint_tail_but_transcript_keeps_original_entries -- --nocapture
cargo test -p threadlane-session compaction_persistence_failure_blocks_provider -- --nocapture
cargo test -p threadlane-session ineffective_compaction_retries_once -- --nocapture
cargo test -p threadlane-session provider_overflow_retries_once -- --nocapture
```

Expected: FAIL because durable preparation is not installed and no context telemetry exists.

- [ ] **Step 3: Enrich provider trace persistence**

Update `CodingSessionHarness::record_provider_trace` so `ProviderTraceEvent::ContextManifest` fills all optional metadata fields introduced in Task 4. Keep legacy call sites compiling by obtaining defaults from the event itself. Verify `ProviderRequestStarted` still records the effective model selected by `TurnDriver`.

- [ ] **Step 4: Implement the atomic durable preparation method**

Implement `prepare_provider_boundary` in `harness.rs` with this exact sequence:

```rust
pub fn prepare_provider_boundary(
    &mut self,
    run_id: &str,
    request: ProviderBoundaryRequest,
    config: &AgentConfig,
) -> Result<ProviderBoundaryResult, String> {
    self.ensure_fresh()?;
    let budget = context_budget(&request.model, config);
    let mut current = self.model_context("main")?.messages();
    let pre_tokens = estimate_request_tokens(
        &current,
        request.tool_schema_json.as_deref(),
        config,
    );
    if pre_tokens < budget.trigger_tokens && !request.overflow_recovery {
        return Ok(boundary_result(current, budget, self.compaction_generation(), None));
    }

    let reason = if request.overflow_recovery {
        CompactionReason::OverflowRecovery
    } else {
        CompactionReason::AdaptiveBudget
    };
    let targets = [budget.retained_tail_tokens, budget.strict_retained_tail_tokens];
    for (index, target) in targets.into_iter().enumerate() {
        let Some(prepared) = compact_for_budget(
            &current,
            request.tool_schema_json.as_deref(),
            target,
            config,
        ) else {
            return Err("context preparation could not drop historical messages".into());
        };
        self.commit_prepared_compaction(run_id, &request.model, budget, reason, prepared)?;
        current = self.model_context("main")?.messages();
        let post_tokens = estimate_request_tokens(
            &current,
            request.tool_schema_json.as_deref(),
            config,
        );
        if post_tokens < budget.trigger_tokens {
            return Ok(boundary_result(
                current,
                budget,
                self.compaction_generation(),
                Some(post_tokens),
            ));
        }
        if index == 1 {
            return Err(format!(
                "context remains above budget after strict compaction: {post_tokens}/{}",
                budget.trigger_tokens,
            ));
        }
        self.ensure_fresh()?;
    }
    unreachable!()
}
```

Add these private helpers in the same `CodingSessionHarness` implementation:

```rust
fn compaction_generation(&self) -> u64 {
    self.store
        .store()
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::ContextCompacted { generation, .. } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn boundary_result(
    messages: Vec<AgentMessage>,
    budget: ContextBudget,
    compaction_generation: u64,
    provisional_estimated_tokens: Option<usize>,
) -> ProviderBoundaryResult {
    ProviderBoundaryResult {
        messages,
        context_limit: budget.limit,
        context_limit_is_estimate: budget.limit_is_estimate,
        compaction_generation,
        provisional_estimated_tokens,
    }
}
```

Define `commit_prepared_compaction` with the exact signature:

```rust
fn commit_prepared_compaction(
    &mut self,
    parent_run_id: &str,
    model: &str,
    budget: ContextBudget,
    reason: CompactionReason,
    prepared: PreparedCompaction,
) -> Result<(), String>
```

Define `CodingSessionHarness::checkpoint_open_run_compaction(run_id, summary, reason)` as the session wrapper over `CompactionProcedure::checkpoint_open_run`. The wrapper drives gated effects before returning; `commit_prepared_compaction` then appends `compaction_retained_tail(&prepared.messages)`, re-projects model context, and appends `Record::ContextCompacted` with generation `self.compaction_generation() + 1`.

`commit_prepared_compaction` uses the still-open foreground `parent_run_id`; it must not start a nested operation or finish the foreground run. Call `checkpoint_open_run_compaction`, drive gated effects to completion, append the retained tail once, re-project, and only then append `Record::ContextCompacted`. If any step fails, call `ensure_fresh` before returning the error. Derive generation as the maximum committed main-lane `ContextCompacted.generation + 1`; this makes reload and retry idempotent.

Do not hold the harness mutex across provider network work; the preparer holds it only for this synchronous persistence/re-projection operation.

- [ ] **Step 5: Install and remove the preparer with run recorders**

In `install_run_trace_recorders`, clone the existing `Arc<tokio::sync::Mutex<CodingSessionHarness>>`, `run_id`, and `AgentConfig`, then call:

```rust
self.agent.set_provider_boundary_preparer(Some(Arc::new(move |request| {
    let harness = boundary_harness.clone();
    let run_id = boundary_run_id.clone();
    let config = boundary_config.clone();
    Box::pin(async move {
        harness
            .lock()
            .await
            .prepare_provider_boundary(&run_id, request, &config)
    })
})));
```

Clear it beside trace/message recorders in `finish_harness_run`. Remove the prompt-only `auto_compact_history` block from `CodingAgent::prompt` only after this hook is installed for accepted durable runs. Manual compaction continues through `compact_history_with_harness` and records `CompactionReason::Manual` telemetry.

- [ ] **Step 6: Handle cancellation without partial operations**

Check cancellation before accepting compaction. Once accepted, always drive gated effects to a terminal state and synchronize canonical messages before returning cancellation. Add an assertion that no `ProviderRequestStarted` follows the cancellation record in the fixture.

- [ ] **Step 7: Run focused and full session tests**

```bash
cargo test -p threadlane-session adaptive_compaction_commits_before_next_provider_attempt -- --nocapture
cargo test -p threadlane-session reload_uses_checkpoint_tail_but_transcript_keeps_original_entries -- --nocapture
cargo test -p threadlane-session compaction_persistence_failure_blocks_provider -- --nocapture
cargo test -p threadlane-session ineffective_compaction_retries_once -- --nocapture
cargo test -p threadlane-session provider_overflow_retries_once -- --nocapture
cargo test -p threadlane-session
```

Expected: PASS. Inspect the ordering assertion rather than relying only on request counts.

- [ ] **Step 8: Commit**

```bash
git add crates/threadlane-session/src/coding_agent/harness.rs crates/threadlane-session/src/coding_agent/durable.rs crates/threadlane-session/src/coding_agent/runtime.rs
git commit -m "feat(session): compact durable context between attempts"
```

---

### Task 6: Project Current Context and Durable Markers in GPUI State

**Files:**
- Modify: `crates/threadlane-gpui/src/state/app_state.rs`
- Test: inline tests in `crates/threadlane-gpui/src/state/app_state.rs`

**Interfaces:**
- Consumes: Task 4 manifest fields, `TranscriptItem`, and `ContextCompactedMarker`.
- Produces: `pub(crate) struct ContextWindowInfo` and `AppState::active_context_window()`.
- Produces: `MessageRole::ContextMarker`, used by Task 7.

- [ ] **Step 1: Write failing projection tests**

Add fixtures for a session with 11.7M cumulative usage, a latest 103.7k manifest, and one prior compaction:

```rust
#[test]
fn context_window_uses_latest_manifest_not_cumulative_usage() {
    let state = state_from_context_fixture();
    let context = state.active_context_window().unwrap();
    assert_eq!(context.current_tokens, 103_732);
    assert_eq!(context.context_limit, 1_000_000);
    assert_eq!(context.effective_model, "gpt-5.6-sol");
    assert!(!context.context_limit_is_estimate);
    assert_eq!(state.active_session_metrics().billed_input_tokens(), 11_734_912);
}

#[test]
fn transcript_marker_survives_reload_without_summary_content() {
    let page = compute_message_page(&context_fixture_path(), None, 0).unwrap();
    assert!(page.messages.iter().any(|message| {
        message.role == MessageRole::ContextMarker
            && message.content == "Context compacted · 742k → 118k"
    }));
    assert!(!page.messages.iter().any(|message| message.content.contains("Summary of prior conversation")));
}

#[test]
fn legacy_session_without_compaction_has_no_fabricated_marker() {
    let state = state_from_legacy_manifest_fixture();
    assert!(state.messages.iter().all(|message| message.role != MessageRole::ContextMarker));
    assert_eq!(state.active_context_window().unwrap().last_compacted_at, None);
}
```

Add model-switch coverage: latest persisted request remains labeled with its model; a transient `estimating` state does not compare it with a newly selected unused model.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p threadlane-gpui context_window_uses_latest_manifest_not_cumulative_usage -- --nocapture
cargo test -p threadlane-gpui transcript_marker_survives_reload_without_summary_content -- --nocapture
cargo test -p threadlane-gpui legacy_session_without_compaction_has_no_fabricated_marker -- --nocapture
```

Expected: FAIL because `ContextWindowInfo`, marker role, and latest-manifest projection do not exist.

- [ ] **Step 3: Add current-context projection state**

Add:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextWindowInfo {
    pub(crate) current_tokens: u64,
    pub(crate) context_limit: u64,
    pub(crate) context_limit_is_estimate: bool,
    pub(crate) effective_model: String,
    pub(crate) compaction_generation: u64,
    pub(crate) last_compacted_at: Option<u64>,
    pub(crate) provisional: bool,
    pub(crate) estimating: bool,
}
```

Add `context_windows: HashMap<String, ContextWindowInfo>` to `AppState` and `context_window` to `SessionProjectionResult`. In `project_trajectory_from_store`, select the highest-sequence main-lane `ContextManifestCaptured`. Prefer its persisted effective model/limit; for a legacy manifest, join `(run_id, attempt, request_id)` to `ProviderRequestStarted.model` and resolve the shared runtime model limit. Select the newest `ContextCompacted` generation; if it is newer than the latest manifest generation, use `post_tokens` provisionally. Do not derive current context from `durable_usage`.

Set `last_compacted_at` only from a real telemetry record. Keep `SessionMetricsInfo` accumulation unchanged.

- [ ] **Step 4: Project transcript items into distinct chat rows**

Update `compute_message_page` to map each `TranscriptItem::Message` through existing `project_agent_messages` and each marker to:

```rust
ChatMessageInfo {
    id: format!("history-{page_serial}-context-{}", marker.seq),
    role: MessageRole::ContextMarker,
    content: format!(
        "Context compacted · {} → {}",
        crate::model_catalog::format_tokens(marker.pre_tokens.min(u32::MAX as usize) as u32),
        crate::model_catalog::format_tokens(marker.post_tokens.min(u32::MAX as usize) as u32),
    ),
    tool_activities: Vec::new(),
    streaming: false,
    reasoning_content: None,
    reasoning_expanded: false,
}
```

Preserve item order and stable IDs across paging/reload. Ensure `ContextMarker` is never folded into assistant tool-activity grouping.

- [ ] **Step 5: Run state projection tests**

```bash
cargo test -p threadlane-gpui state::app_state::tests -- --nocapture
```

Expected: PASS, including existing exact-usage and paged-history tests.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-gpui/src/state/app_state.rs
git commit -m "feat(gpui): project current context telemetry"
```

---

### Task 7: Render Truthful Meter and Compaction Timeline Marker

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/chat/view.rs`
- Test: inline `hot_path_tests` in `view.rs`

**Interfaces:**
- Consumes: `ContextWindowInfo`, cumulative `SessionMetricsInfo`, and `MessageRole::ContextMarker` from Task 6.
- Produces: pure formatting helper `context_meter_view_model` so semantics are unit-testable without pixel assertions.

- [ ] **Step 1: Write failing meter-view-model tests**

Add a small value type/helper and tests:

```rust
#[test]
fn meter_separates_current_context_from_total_processed() {
    let view = context_meter_view_model(
        Some(&ContextWindowInfo {
            current_tokens: 103_732,
            context_limit: 1_000_000,
            context_limit_is_estimate: false,
            effective_model: "gpt-5.6-sol".into(),
            compaction_generation: 0,
            last_compacted_at: None,
            provisional: false,
            estimating: false,
        }),
        &metrics_with_usage(266_614, 30_285, 11_734_912, 0),
    );
    assert_eq!(view.percent, Some(10.3732));
    assert_eq!(view.current_label, "103.7k / 1.0M");
    assert_eq!(view.total_processed_label, "12.0M");
    assert_eq!(view.cache_hit_label.as_deref(), Some("98%"));
}

#[test]
fn estimating_context_has_no_false_percentage() {
    let view = context_meter_view_model(Some(&estimating_context()), &SessionMetricsInfo::default());
    assert_eq!(view.percent, None);
    assert_eq!(view.current_label, "Estimating…");
}
```

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p threadlane-gpui screens::chat::view::hot_path_tests::meter_ -- --nocapture
```

Expected: FAIL because the helper/value model does not exist.

- [ ] **Step 3: Implement pure meter semantics**

The helper computes percentage only when `!estimating && context_limit > 0`, clamps only the rendered bar to 100%, labels estimated limits, uses provisional post-compaction values transparently, and calculates Total processed exactly as current `billed_input_tokens + output_tokens`. Do not use `current_session_token_usage()` in the composer meter.

- [ ] **Step 4: Update hover card and bar**

In `render_composer`:

- read `active_context_window()` and `active_session_metrics()` once;
- drive bar/percentage/warning colors from `ContextWindowInfo.current_tokens / context_limit`;
- show effective model context label when helpful;
- show `Total processed` and `Cache hit` as separate cumulative rows;
- show `Last compacted` only when telemetry exists;
- use `Estimating…` after preparation begins for a new effective model;
- retain the compact explanatory copy `Context is compacted automatically when needed.`.

For `MessageRole::ContextMarker`, render a centered muted timeline row with no assistant avatar, markdown parsing, copy controls, or tool grouping. Its copy is exactly `Context compacted · 742k → 118k`; Task 6's marker content is the complete first-release display, so no additional marker-detail state is introduced.

- [ ] **Step 5: Run focused GPUI tests and check**

```bash
cargo test -p threadlane-gpui screens::chat::view::hot_path_tests -- --nocapture
cargo check -p threadlane-gpui
```

Expected: PASS. Do not claim visual verification unless the application is run and observed.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-gpui/src/screens/chat/view.rs
git commit -m "feat(gpui): show current model context"
```

---

### Task 8: Integration Regression, Durable Guidance, and Final Verification

**Files:**
- Modify: `crates/threadlane-session/src/coding_agent/runtime.rs` test module for the 102-attempt runtime regression
- Modify: `crates/threadlane-gpui/src/state/app_state.rs` test module for the cumulative-usage/current-context regression
- Modify: `AGENTS.md`

**Interfaces:**
- Validates all prior task interfaces together; produces no new production abstraction.

- [ ] **Step 1: Add the reported-session-shape regression test**

Create `long_cached_tool_loop_compacts_before_budget` in the `threadlane-session` test module with one foreground run, 102 attempts, repeated tool results, and no intervening user prompt. Assert:

```rust
assert_eq!(provider.attempts(), 102);
assert!(provider.max_request_estimate() < provider.context_budget());
assert!(store.records().iter().any(|record| matches!(record,
    Record::ContextCompacted { reason: CompactionReason::AdaptiveBudget, .. }
)));
```

Create `reported_session_shape_keeps_total_processed_separate` in `threadlane-gpui/src/state/app_state.rs` with cumulative cache-read usage greater than the model limit and a latest manifest below it. Assert:

```rust
assert!(projected_metrics.billed_input_tokens() > projected_context.context_limit);
assert!(projected_context.current_tokens < projected_context.context_limit);
assert_eq!(projected_context.current_tokens, 103_732);
```

- [ ] **Step 2: Run both regressions and verify failure before fixture correction**

```bash
cargo test -p threadlane-session long_cached_tool_loop_compacts_before_budget -- --nocapture
cargo test -p threadlane-gpui reported_session_shape_keeps_total_processed_separate -- --nocapture
```

Expected on first run: FAIL if cross-crate wiring, request estimates, telemetry ordering, or UI projection remains incomplete. Correct only the responsible prior-task implementation; do not weaken the assertions.

- [ ] **Step 3: Record the durable repository invariant**

Add under `Session and Context-Menu Behavior` in `AGENTS.md`:

```markdown
- Durable coding sessions prepare context at every provider-attempt boundary, including tool-loop attempts. Required compaction must commit its harness branch and telemetry before `ProviderRequestStarted`; never rewrite only `TurnDriver` state or derive current-context UI from cumulative usage.
```

- [ ] **Step 4: Run focused crate suites**

```bash
cargo test -p threadlane-runtime
cargo test -p threadlane-session
cargo test -p threadlane-gpui state::app_state::tests -- --nocapture
cargo test -p threadlane-gpui screens::chat::view::hot_path_tests -- --nocapture
```

Expected: all pass.

- [ ] **Step 5: Run required workspace-facing validation**

```bash
cargo check -p threadlane-gpui
git diff --check
```

Expected: both pass with no new errors and no whitespace defects. If shared metadata/schema changes expose cross-crate failures, also run:

```bash
cargo test --workspace
```

- [ ] **Step 6: Review durable compatibility and accidental changes**

Run:

```bash
git status --short
git diff --stat HEAD~7..HEAD
git diff --check HEAD~7..HEAD
```

Confirm no `.threadlane/`, `target/`, private key, credential, or unrelated generated file is present. Confirm old JSONL fixture tests pass and all new fields are serde-defaulted.

- [ ] **Step 7: Commit final regression and guidance**

```bash
git add AGENTS.md crates/threadlane-session/src/coding_agent/runtime.rs crates/threadlane-gpui/src/state/app_state.rs
git commit -m "test: cover long-running context compaction"
```

- [ ] **Step 8: Final evidence summary**

Report exact commands and outcomes. Include the key behavioral evidence: compaction telemetry sequence precedes the next provider-start sequence; latest-manifest context is below its limit while Total processed may exceed it; transcript marker and original history survive reload. State explicitly whether `cargo test --workspace` and visual app verification were or were not run.
