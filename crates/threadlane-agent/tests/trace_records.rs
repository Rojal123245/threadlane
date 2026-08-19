use serde_json::{json, Value};
use threadlane_agent::harness::{
    EventPayload, HarnessEventHub, MemoryStore, OperationIntent, Record, ReduceError, Reducer,
};

fn record(variant: &str, fields: Value) -> Record {
    serde_json::from_value(json!({ variant: fields })).unwrap()
}

fn base(id: &str, seq: u64) -> Value {
    json!({
        "id": id,
        "seq": seq,
        "lane": "main",
        "timestamp": 1_700_000_000_u64,
        "run_id": "run-1",
        "attempt": 1
    })
}

#[test]
fn trace_record_variants_round_trip_without_generic_payloads() {
    let mut cases = Vec::new();

    let mut fields = base("context", 1);
    fields["attempt"] = Value::Null;
    fields["model"] = json!("gpt-test");
    fields["provider"] = json!("openai");
    fields["reasoning_effort"] = json!("medium");
    fields["prompt_cache_enabled"] = json!(true);
    fields["work_dir"] = json!("/workspace");
    fields["system_prompt"] = json!({
        "Full": {
            "content": "safe fixture prompt",
            "sha256": "sha256:prompt"
        }
    });
    fields["tool_schema_sha256"] = json!("sha256:tools");
    fields["enabled_tool_names"] = json!(["read_file"]);
    fields["capabilities"] = json!({
        "capabilities": ["tool:read_file", "skill:rust"],
        "fingerprint": "sha256:capabilities"
    });
    fields["prompt_template_ids"] = json!(["review"]);
    fields["git_head"] = json!("deadbeef");
    cases.push(("RunContextCaptured", fields));

    let mut fields = base("provider-start", 2);
    fields["provider"] = json!("openai");
    fields["model"] = json!("gpt-test");
    fields["request_id"] = json!("request-1");
    cases.push(("ProviderRequestStarted", fields));

    let mut fields = base("provider-finish", 3);
    fields["request_id"] = json!("request-1");
    fields["outcome"] = json!("Failed");
    fields["error"] = json!({
        "category": "RateLimit",
        "code": "rate_limit_exceeded",
        "retryable": true
    });
    fields["duration_ms"] = json!(125);
    fields["usage"] = Value::Null;
    cases.push(("ProviderRequestFinished", fields));

    let mut fields = base("provider-response", 31);
    fields["request_id"] = json!("request-1");
    fields["entry_id"] = json!("entry-assistant-1");
    fields["reasoning_entry_id"] = json!("entry-thinking-1");
    cases.push(("ProviderResponseAttached", fields));

    let mut fields = base("permission-request", 4);
    fields["run_id"] = Value::Null;
    fields["attempt"] = Value::Null;
    fields["request_id"] = json!("permission-1");
    fields["capability"] = json!("network");
    fields["scopes"] = json!(["Once", "Project"]);
    fields["detail_sha256"] = json!("sha256:detail");
    fields["source"] = json!("User");
    cases.push(("PermissionRequested", fields));

    let mut fields = base("permission-resolved", 5);
    fields["request_id"] = json!("permission-1");
    fields["decision"] = json!("Allowed");
    fields["scope"] = json!("Once");
    fields["source"] = json!("User");
    fields["remembered"] = json!(false);
    cases.push(("PermissionResolved", fields));

    let mut fields = base("tool-observed", 6);
    fields["tool_call_id"] = json!("call-1");
    fields["tool_name"] = json!("read_file");
    fields["executor_kind"] = json!("builtin");
    fields["phase"] = json!("Finished");
    fields["started_at_ms"] = json!(1_700_000_000_000_u64);
    fields["duration_ms"] = json!(8);
    fields["outcome"] = json!("Succeeded");
    fields["exit_code"] = Value::Null;
    fields["cancelled"] = json!(false);
    fields["is_error"] = json!(false);
    fields["terminate"] = json!(false);
    fields["output_sha256"] = json!("sha256:output");
    fields["output_bytes"] = json!(128);
    cases.push(("ToolExecutionObserved", fields));

    let mut fields = base("abort-observed", 7);
    fields["observation"] = json!("ProviderNotified");
    fields["initiator"] = json!("User");
    fields["target"] = json!("Provider");
    fields["acknowledged"] = json!(true);
    fields["detail"] = Value::Null;
    cases.push(("AbortObserved", fields));

    let mut fields = base("subagent", 8);
    fields["child_run_id"] = json!("child-1");
    fields["parent_tool_call_id"] = Value::Null;
    fields["task_index"] = json!(0);
    fields["agent_id"] = json!("reviewer");
    fields["subagent_lane"] = json!("child@1");
    fields["phase"] = json!("Started");
    fields["result_entry_id"] = Value::Null;
    fields["error"] = Value::Null;
    cases.push(("SubagentLifecycle", fields));

    let mut fields = base("checkpoint", 9);
    fields["request_id"] = json!("request-1");
    fields["assistant_entry_id"] = Value::Null;
    fields["text"] = json!("partial response");
    fields["reasoning"] = Value::Null;
    fields["checkpoint_index"] = json!(1);
    fields["byte_count"] = json!(16);
    fields["fingerprint"] = json!("sha256:stream");
    cases.push(("StreamCheckpoint", fields));

    for (variant, fields) in cases {
        let encoded = json!({ variant: fields });
        let decoded: Record = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.id(), encoded[variant]["id"].as_str().unwrap());
        assert_eq!(decoded.seq(), encoded[variant]["seq"].as_u64().unwrap());
        assert_eq!(decoded.lane(), "main");
        assert_eq!(decoded.run_id(), encoded[variant]["run_id"].as_str());
        assert_eq!(
            decoded.turn(),
            encoded[variant]["attempt"].as_u64().map(|n| n as u32)
        );
        assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded);

        let event = HarnessEventHub::new(1).publish(EventPayload::RecordCommitted(decoded));
        assert_eq!(event.payload_variant(), variant);
        assert!(event.project_agent_event().is_none());
    }
}

#[test]
fn legacy_records_remain_readable_and_abort_requested_shape_is_unchanged() {
    let abort = json!({
        "AbortRequested": {
            "id": "abort-1",
            "seq": 2,
            "lane": "main",
            "timestamp": 2,
            "run_id": "run-1"
        }
    });
    let decoded: Record = serde_json::from_value(abort.clone()).unwrap();
    assert!(matches!(decoded, Record::AbortRequested { .. }));
    assert_eq!(serde_json::to_value(decoded).unwrap(), abort);

    let started = json!({
        "OperationStarted": {
            "id": "run-1",
            "seq": 1,
            "lane": "main",
            "timestamp": 1,
            "source_leaf_id": null,
            "intent": "Run"
        }
    });
    let decoded: Record = serde_json::from_value(started.clone()).unwrap();
    assert!(matches!(decoded, Record::OperationStarted { .. }));
    assert_eq!(serde_json::to_value(decoded).unwrap(), started);
}

#[test]
fn observational_records_preserve_reduced_state_and_validate_active_run_identity() {
    let mut store = MemoryStore::new("trace-session");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });

    store.append_record(record(
        "StreamCheckpoint",
        json!({
            "id": "checkpoint-1",
            "seq": 2,
            "lane": "main",
            "timestamp": 2,
            "run_id": "run-1",
            "attempt": 1,
            "request_id": "request-1",
            "assistant_entry_id": null,
            "text": "partial response",
            "reasoning": null,
            "checkpoint_index": 1,
            "byte_count": 16,
            "fingerprint": "sha256:stream"
        }),
    ));

    let state = Reducer::reduce(&store).unwrap();
    let lane = state.lane("main").unwrap();
    assert_eq!(lane.open_operation.as_deref(), Some("run-1"));
    assert_eq!(lane.attempts, 0);

    let mismatch = record(
        "AbortObserved",
        json!({
            "id": "abort-other",
            "seq": 3,
            "lane": "main",
            "timestamp": 3,
            "run_id": "run-other",
            "attempt": null,
            "observation": "Confirmed",
            "initiator": "Recovery",
            "target": "ActiveRun",
            "acknowledged": true,
            "detail": null
        }),
    );
    assert_eq!(
        store.try_append_record(mismatch),
        Err(ReduceError::UnknownOperation("run-other".into()))
    );
}

#[test]
fn trace_records_obey_strict_global_sequence_ordering() {
    let mut store = MemoryStore::new("trace-sequence");
    store.append_record(record(
        "PermissionRequested",
        json!({
            "id": "permission-1",
            "seq": 4,
            "lane": "main",
            "timestamp": 4,
            "run_id": null,
            "attempt": null,
            "request_id": "request-1",
            "capability": "network",
            "scopes": ["Once"],
            "detail_sha256": "sha256:detail",
            "source": "UnattendedDefault"
        }),
    ));

    let duplicate_sequence = record(
        "PermissionResolved",
        json!({
            "id": "permission-2",
            "seq": 4,
            "lane": "main",
            "timestamp": 5,
            "run_id": null,
            "attempt": null,
            "request_id": "request-1",
            "decision": "Denied",
            "scope": null,
            "source": "UnattendedDefault",
            "remembered": false
        }),
    );
    assert_eq!(
        store.try_append_record(duplicate_sequence),
        Err(ReduceError::NonMonotonicSequence {
            previous: 4,
            current: 4
        })
    );
}

#[test]
fn trace_strings_reject_unbounded_values() {
    let oversized = "x".repeat(4097);
    let value = json!({
        "ProviderRequestStarted": {
            "id": "provider-start",
            "seq": 1,
            "lane": "main",
            "timestamp": 1,
            "run_id": "run-1",
            "attempt": 1,
            "provider": oversized,
            "model": "gpt-test",
            "request_id": null
        }
    });
    assert!(serde_json::from_value::<Record>(value).is_err());
}

#[test]
fn bounded_prompt_text_rejects_oversized_values() {
    let oversized = "x".repeat(256 * 1024 + 1);
    assert!(threadlane_agent::harness::BoundedPromptText::new(&oversized).is_err());

    let value = json!({
        "Full": {
            "content": oversized,
            "sha256": "sha256:prompt"
        }
    });
    assert!(serde_json::from_value::<threadlane_agent::harness::PromptSnapshot>(value).is_err());

    let valid = "safe prompt content";
    let bounded = threadlane_agent::harness::BoundedPromptText::new(valid).unwrap();
    assert_eq!(bounded.as_str(), valid);
}

#[test]
fn tool_arguments_sanitizer_redacts_secrets_and_bounds_strings() {
    let long_payload = "a".repeat(100 * 1024);
    let raw_args = json!({
        "api_key": "sk-secret-token-12345",
        "nested": {
            "password": "super-secret-password",
            "bearer_token": "bearer-xyz",
            "auth_header": "Basic abc"
        },
        "safe_key": "normal-value",
        "oversized_data": long_payload
    });

    let sanitized = threadlane_agent::harness::sanitize_tool_args(&raw_args);
    assert_eq!(sanitized["api_key"], "[REDACTED]");
    assert_eq!(sanitized["nested"]["password"], "[REDACTED]");
    assert_eq!(sanitized["nested"]["bearer_token"], "[REDACTED]");
    assert_eq!(sanitized["nested"]["auth_header"], "[REDACTED]");
    assert_eq!(sanitized["safe_key"], "normal-value");

    let oversized_str = sanitized["oversized_data"].as_str().unwrap();
    assert!(oversized_str.contains("[TRUNCATED"));
    assert!(oversized_str.len() <= 64 * 1024 + 64);
}
