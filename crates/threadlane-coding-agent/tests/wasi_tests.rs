use serde_json::Value;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use threadlane_agent::harness::HookContext;
use threadlane_coding_agent::{
    extension_before_tool_hook_handler, BrokerError, BrokerOperationResult, BrokerRequest,
    CapabilityCatalog, CapabilityDispatcher, CapabilityHandler, CapabilityPolicy, ExtensionManager,
    ExtensionScope, HostBrokerRequest, HostCapabilityGrantPolicy, ToolPolicy, WasiExtension,
    WasiExtensionEvent, WasiExtensionManager, WasiExtensionManifest, WasiToolDefinition,
};

fn build_broker_smoke_extension(agent_only: bool) -> PathBuf {
    static BUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = BUILD_LOCK.lock().unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target_dir = root.join(if agent_only {
        "target/broker-smoke-agent"
    } else {
        "target/broker-smoke-tools"
    });
    let wasm_path = target_dir.join("wasm32-wasip1/debug/broker_smoke_ext.wasm");
    let mut command = Command::new("cargo");
    command
        .current_dir(&root)
        .args([
            "build",
            "--manifest-path",
            "extensions/broker_smoke_ext/Cargo.toml",
            "--target",
            "wasm32-wasip1",
            "--target-dir",
        ])
        .arg(&target_dir);
    if agent_only {
        command.args(["--features", "agent-only"]);
    }
    assert!(command.status().unwrap().success());
    wasm_path
}

#[test]
fn broker_smoke_manifest_matches_v2_documentation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(".threadlane/extensions/broker_smoke_ext.wasm");
    let extension = WasiExtension::load_from_file(&path).unwrap_or_else(|error| {
        panic!(
            "load deployed broker smoke extension at {} (run scripts/build_extensions.sh): {error}",
            path.display()
        )
    });

    assert_eq!(extension.manifest.api_version, 2);
    assert_eq!(extension.manifest.capabilities, vec!["tools"]);
    assert_eq!(
        extension
            .manifest
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["broker-smoke"]
    );
}

#[test]
fn debug_extension_manifest_declares_dap_tools_and_process_capability() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(".threadlane/extensions/debug_ext.wasm");
    let extension = WasiExtension::load_from_file(&path).unwrap_or_else(|error| {
        panic!(
            "load deployed debug extension at {} (run scripts/build_extensions.sh): {error}",
            path.display()
        )
    });

    assert_eq!(extension.manifest.api_version, 2);
    assert_eq!(extension.manifest.name, "debug_ext");
    // Debugging drives an adapter subprocess and nothing else; a wider grant
    // would hand a third-party adapter capabilities it never needs.
    assert_eq!(extension.manifest.capabilities, vec!["process"]);
    assert_eq!(
        extension
            .manifest
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["debug_run", "debug_continue", "debug_eval", "debug_stop"]
    );
}

#[test]
fn broker_import_queues_accepted_requests_and_returns_denials_to_the_extension() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();

    let result = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert!(result.message.contains("broker accepted tools.set_policy"));
    assert_eq!(result.broker_requests.len(), 1);
    assert_eq!(result.broker_requests[0].capability, "tools");
    assert_eq!(result.broker_requests[0].operation, "set_policy");

    let malformed = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"malformed"}"#)
        .unwrap()
        .unwrap();
    assert!(malformed.message.contains("invalid_request"));
    assert!(malformed.broker_requests.is_empty());

    let too_small = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"small-output"}"#)
        .unwrap()
        .unwrap();
    assert!(too_small.message.contains("broker response too large"));
    assert_eq!(too_small.broker_requests.len(), 1);

    let denied = WasiExtension::load_from_file(&build_broker_smoke_extension(true)).unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(denied).unwrap();
    let result = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert!(result.message.contains("capability_denied"));
    assert!(result.broker_requests.is_empty());
}

#[tokio::test]
async fn wasm_extension_receives_broker_response_on_next_invocation() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();

    let initial = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"result-event"}"#)
        .unwrap()
        .unwrap();
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register(
        "tools",
        Arc::new(RecordingCapabilityHandler {
            recorded: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    let dispatch = dispatcher
        .dispatch_envelopes(initial.host_broker_requests)
        .await
        .unwrap();
    manager.enqueue_broker_results(dispatch.operation_results);

    let next = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert!(next.message.contains("received broker_response event"));
}

#[test]
fn restrictive_host_grant_denies_declared_capability() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let manager = WasiExtensionManager::with_capability_grant_policy(
        HostCapabilityGrantPolicy::restrict_to(["agent"]),
    );
    manager.register_extension(extension).unwrap();

    let result = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert!(result.message.contains("capability_denied"));
    assert!(result.broker_requests.is_empty());
    assert!(manager.take_pending_broker_requests().is_empty());
}

struct OutputCapabilityHandler;

impl CapabilityHandler for OutputCapabilityHandler {
    fn handle(&self, _request: &BrokerRequest) -> Result<Value, BrokerError> {
        Ok(serde_json::json!({"follow_up_prompt":"must be asynchronous"}))
    }
}

#[tokio::test]
async fn broker_outputs_are_queued_as_future_invocation_events() {
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register("agent", Arc::new(OutputCapabilityHandler));
    let result = dispatcher
        .dispatch_envelopes(vec![HostBrokerRequest {
            request: BrokerRequest {
                api_version: 2,
                capability: "agent".into(),
                operation: "request_turn".into(),
                arguments: Value::Null,
            },
            invoking_extension: "extension".into(),
        }])
        .await
        .unwrap();

    assert_eq!(result.operation_results.len(), 1);

    let manager = WasiExtensionManager::new();
    manager.enqueue_broker_results(result.operation_results);
    assert_eq!(
        manager.drain_events_for("extension").unwrap(),
        vec![WasiExtensionEvent {
            topic: "broker_response".into(),
            payload: serde_json::json!({
                "api_version": 2,
                "capability": "agent",
                "operation": "request_turn",
                "arguments": null,
                "ok": true,
                "value": {"follow_up_prompt":"must be asynchronous"}
            }),
        }]
    );
}

fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        bytes.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
    wasm.push(id);
    push_unsigned_leb(payload.len() as u32, wasm);
    wasm.extend_from_slice(payload);
}

fn manifest_wasm(json: &str) -> Vec<u8> {
    let mut wasm = b"\0asm\x01\0\0\0".to_vec();

    push_section(&mut wasm, 1, &[1, 0x60, 0, 1, 0x7e]);
    push_section(&mut wasm, 3, &[1, 0]);
    push_section(&mut wasm, 5, &[1, 0, 1]);

    let mut exports = vec![2];
    push_unsigned_leb("extension_info".len() as u32, &mut exports);
    exports.extend_from_slice(b"extension_info");
    exports.extend_from_slice(&[0, 0]);
    push_unsigned_leb("memory".len() as u32, &mut exports);
    exports.extend_from_slice(b"memory");
    exports.extend_from_slice(&[2, 0]);
    push_section(&mut wasm, 7, &exports);

    let mut body = vec![0, 0x42];
    push_signed_leb(json.len() as i64, &mut body);
    body.push(0x0b);
    let mut code = vec![1];
    push_unsigned_leb(body.len() as u32, &mut code);
    code.extend_from_slice(&body);
    push_section(&mut wasm, 10, &code);

    let mut data = vec![1, 0, 0x41, 0, 0x0b];
    push_unsigned_leb(json.len() as u32, &mut data);
    data.extend_from_slice(json.as_bytes());
    push_section(&mut wasm, 11, &data);
    wasm
}

#[test]
fn scoped_wasi_inventory_runtime_and_catalog_share_precedence() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let global_extensions = global_threadlane.path().join("extensions");
    let project_extensions = project.path().join(".threadlane/extensions");
    std::fs::create_dir_all(global_extensions.join("managed-global")).unwrap();
    std::fs::create_dir_all(project_extensions.join("managed-project")).unwrap();
    std::fs::write(
        global_extensions.join("shared_ext.wasm"),
        manifest_wasm(
            r#"{"api_version":1,"name":"shared_ext","version":"1.0.0","description":"global"}"#,
        ),
    )
    .unwrap();
    std::fs::write(
        global_extensions.join("managed-global/extension.wasm"),
        manifest_wasm(
            r#"{"api_version":1,"name":"global_only","version":"1.0.0","description":"global"}"#,
        ),
    )
    .unwrap();
    std::fs::write(
        project_extensions.join("managed-project/extension.wasm"),
        manifest_wasm(
            r#"{"api_version":1,"name":"shared_ext","version":"2.0.0","description":"project"}"#,
        ),
    )
    .unwrap();
    std::fs::write(
        project_extensions.join("disabled_ext.wasm"),
        manifest_wasm(
            r#"{"api_version":1,"name":"disabled_ext","version":"1.0.0","description":"disabled"}"#,
        ),
    )
    .unwrap();
    std::fs::write(project_extensions.join("disabled_ext.wasm.disabled"), []).unwrap();

    let lifecycle = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    let inventory = lifecycle.discover();
    assert_eq!(inventory.len(), 4);
    assert!(inventory.iter().any(|record| {
        record.name() == "shared_ext"
            && record.scope() == ExtensionScope::Global
            && !record.is_effective()
    }));
    assert!(inventory.iter().any(|record| {
        record.name() == "shared_ext"
            && record.scope() == ExtensionScope::Project
            && record.is_effective()
    }));
    assert!(inventory.iter().any(|record| {
        record.name() == "disabled_ext" && !record.is_enabled() && !record.is_effective()
    }));

    let runtime = WasiExtensionManager::for_project(project.path());
    assert_eq!(
        runtime
            .reload_from_roots(Some(global_threadlane.path()), Some(project.path()))
            .unwrap(),
        2
    );
    assert_eq!(
        runtime.extension_manifest("shared_ext").unwrap().version,
        "2.0.0"
    );
    assert!(runtime.extension_manifest("global_only").is_some());
    assert!(runtime.extension_manifest("disabled_ext").is_none());

    let catalog = CapabilityCatalog::discover_with_roots(
        Some(global_threadlane.path()),
        Some(project.path()),
    );
    assert_eq!(catalog.extensions().len(), inventory.len());
    assert_eq!(
        catalog
            .extensions()
            .iter()
            .filter(|record| record.is_effective())
            .count(),
        2
    );

    let project_override = inventory
        .iter()
        .find(|record| record.name() == "shared_ext" && record.scope() == ExtensionScope::Project)
        .unwrap();
    lifecycle.set_enabled(project_override, false).unwrap();
    std::fs::write(project_override.module_path(), b"disabled invalid wasm").unwrap();
    assert_eq!(
        runtime
            .reload_from_roots(Some(global_threadlane.path()), Some(project.path()))
            .unwrap(),
        2
    );
    assert_eq!(
        runtime.extension_manifest("shared_ext").unwrap().version,
        "1.0.0"
    );
}

#[test]
fn scoped_wasi_failed_reload_keeps_previous_registry() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let module = project
        .path()
        .join(".threadlane/extensions/reload_ext.wasm");
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    std::fs::write(
        &module,
        manifest_wasm(
            r#"{"api_version":1,"name":"reload_ext","version":"1.0.0","description":"initial"}"#,
        ),
    )
    .unwrap();
    let runtime = Arc::new(WasiExtensionManager::for_project(project.path()));
    runtime
        .reload_from_roots(Some(global_threadlane.path()), Some(project.path()))
        .unwrap();
    let consumer = runtime.clone();
    assert_eq!(
        consumer.extension_manifest("reload_ext").unwrap().version,
        "1.0.0"
    );

    std::fs::write(&module, b"invalid wasm").unwrap();
    assert!(runtime
        .reload_from_roots(Some(global_threadlane.path()), Some(project.path()))
        .is_err());
    assert_eq!(
        consumer.extension_manifest("reload_ext").unwrap().version,
        "1.0.0"
    );
}

fn hook_wasm(api_version: u32, response: &str) -> Vec<u8> {
    let manifest = serde_json::json!({
        "api_version": api_version,
        "name": format!("hook_v{api_version}"),
        "version": "1.0.0",
        "description": "test hook",
        "hooks": ["before_tool_call"]
    })
    .to_string();
    let response_offset = 1024usize;
    let manifest_len = manifest.len();
    let mut data_bytes = manifest.into_bytes();
    data_bytes.resize(response_offset, 0);
    data_bytes.extend_from_slice(response.as_bytes());

    let mut wasm = b"\0asm\x01\0\0\0".to_vec();
    push_section(
        &mut wasm,
        1,
        &[
            3, 0x60, 0, 1, 0x7e, // extension_info() -> i64
            0x60, 1, 0x7f, 1, 0x7f, // alloc(i32) -> i32
            0x60, 2, 0x7f, 0x7f, 1, 0x7e, // handle_hook(i32, i32) -> i64
        ],
    );
    push_section(&mut wasm, 3, &[3, 0, 1, 2]);
    push_section(&mut wasm, 5, &[1, 0, 1]);

    let mut exports = vec![4];
    for (name, kind, index) in [
        ("extension_info", 0, 0),
        ("alloc", 0, 1),
        ("handle_hook", 0, 2),
        ("memory", 2, 0),
    ] {
        push_unsigned_leb(name.len() as u32, &mut exports);
        exports.extend_from_slice(name.as_bytes());
        exports.extend_from_slice(&[kind, index]);
    }
    push_section(&mut wasm, 7, &exports);

    let mut bodies = Vec::new();
    for (index, (offset, len)) in [(0, manifest_len), (0, 0), (response_offset, response.len())]
        .into_iter()
        .enumerate()
    {
        let mut body = vec![0];
        if index == 1 {
            body.push(0x41);
            push_signed_leb(offset as i64, &mut body);
        } else {
            body.push(0x42);
            let result = ((offset as u64) << 32) | len as u64;
            push_signed_leb(result as i64, &mut body);
        }
        body.push(0x0b);
        push_unsigned_leb(body.len() as u32, &mut bodies);
        bodies.extend_from_slice(&body);
    }
    let mut code = vec![3];
    code.extend_from_slice(&bodies);
    push_section(&mut wasm, 10, &code);

    let mut data = vec![1, 0, 0x41, 0, 0x0b];
    push_unsigned_leb(data_bytes.len() as u32, &mut data);
    data.extend_from_slice(&data_bytes);
    push_section(&mut wasm, 11, &data);
    wasm
}

#[test]
fn broker_import_rejects_out_of_bounds_huge_request_without_allocating() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();

    let result = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"huge-length"}"#)
        .unwrap()
        .unwrap();
    assert_eq!(result.message, "broker invalid range");
    assert!(result.broker_requests.is_empty());
}

struct RecordingCapabilityHandler {
    recorded: Arc<Mutex<Vec<(String, String)>>>,
}

impl CapabilityHandler for RecordingCapabilityHandler {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        self.recorded
            .lock()
            .unwrap()
            .push((request.capability.clone(), request.operation.clone()));
        if request.operation == "unsupported" {
            return Err(BrokerError {
                code: "unknown_operation".into(),
                message: "unsupported operation".into(),
            });
        }
        Ok(Value::Null)
    }
}

#[tokio::test]
async fn broker_dispatch_routes_capability_operations_in_order() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register(
        "tools",
        Arc::new(RecordingCapabilityHandler {
            recorded: recorded.clone(),
        }),
    );
    let result = dispatcher
        .dispatch(vec![BrokerRequest {
            api_version: 2,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: serde_json::json!({"policy":"read_only"}),
        }])
        .await
        .unwrap();
    assert_eq!(
        *recorded.lock().unwrap(),
        vec![("tools".into(), "set_policy".into())]
    );
    assert_eq!(result.operation_results.len(), 1);
}

#[tokio::test]
async fn broker_dispatch_delivers_failed_outcomes_and_continues() {
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register(
        "tools",
        Arc::new(RecordingCapabilityHandler {
            recorded: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    let outcomes = dispatcher
        .dispatch_envelopes(vec![
            HostBrokerRequest {
                request: BrokerRequest {
                    api_version: 2,
                    capability: "missing".into(),
                    operation: "anything".into(),
                    arguments: Value::Null,
                },
                invoking_extension: "extension".into(),
            },
            HostBrokerRequest {
                request: BrokerRequest {
                    api_version: 2,
                    capability: "tools".into(),
                    operation: "unsupported".into(),
                    arguments: Value::Null,
                },
                invoking_extension: "extension".into(),
            },
            HostBrokerRequest {
                request: BrokerRequest {
                    api_version: 2,
                    capability: "tools".into(),
                    operation: "set_policy".into(),
                    arguments: Value::Null,
                },
                invoking_extension: "extension".into(),
            },
        ])
        .await
        .unwrap();
    assert_eq!(outcomes.operation_results.len(), 3);

    let manager = WasiExtensionManager::new();
    manager.enqueue_broker_results(outcomes.operation_results);
    let events = manager.drain_events_for("extension").unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].payload["error"]["code"], "unknown_capability");
    assert_eq!(events[1].payload["error"]["code"], "unknown_operation");
    assert_eq!(events[2].payload["ok"], true);
}

#[test]
fn broker_results_are_isolated_by_session_scope() {
    let manager = WasiExtensionManager::new();
    manager
        .subscribe_event("listener", "updates".into())
        .unwrap();
    manager.set_session_scope("session-a").unwrap();
    manager
        .publish_event("updates".into(), serde_json::json!({"session":"a"}))
        .unwrap();
    manager.enqueue_broker_results(vec![BrokerOperationResult {
        invoking_extension: "extension".into(),
        request: BrokerRequest {
            api_version: 2,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: Value::Null,
        },
        value: serde_json::json!({"session":"a"}),
        error: None,
    }]);

    manager.set_session_scope("session-b").unwrap();
    assert!(manager.drain_events_for("listener").unwrap().is_empty());
    assert!(manager.drain_events_for("extension").unwrap().is_empty());
    manager.enqueue_broker_results(vec![BrokerOperationResult {
        invoking_extension: "extension".into(),
        request: BrokerRequest {
            api_version: 2,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: Value::Null,
        },
        value: serde_json::json!({"session":"b"}),
        error: None,
    }]);

    manager.set_session_scope("session-a").unwrap();
    assert_eq!(
        manager.drain_events_for("listener").unwrap()[0].payload["session"],
        "a"
    );
    assert_eq!(
        manager.drain_events_for("extension").unwrap()[0].payload["value"]["session"],
        "a"
    );
    manager.set_session_scope("session-b").unwrap();
    assert_eq!(
        manager.drain_events_for("extension").unwrap()[0].payload["value"]["session"],
        "b"
    );
}

#[test]
fn broker_request_round_trips_and_requires_v2() {
    let request: BrokerRequest = serde_json::from_str(
        r#"{"api_version":2,"capability":"tools","operation":"set_policy","arguments":{"policy":"read_only"}}"#,
    )
    .unwrap();
    assert_eq!(request.api_version, 2);
    assert_eq!(request.capability, "tools");
    assert_eq!(request.operation, "set_policy");
}

#[test]
fn events_are_topic_filtered_and_queued_for_next_invocation() {
    let manager = WasiExtensionManager::new();
    manager
        .subscribe_event("listener", "updates".into())
        .unwrap();
    manager
        .publish_event("ignored".into(), serde_json::json!(1))
        .unwrap();
    manager
        .publish_event("updates".into(), serde_json::json!({"ok":true}))
        .unwrap();
    assert_eq!(
        manager.drain_events_for("listener").unwrap(),
        vec![WasiExtensionEvent {
            topic: "updates".into(),
            payload: serde_json::json!({"ok":true}),
        }]
    );
    assert!(manager.drain_events_for("listener").unwrap().is_empty());
}

#[test]
fn actual_extension_invocation_receives_queued_subscribed_events() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let extension_name = extension.manifest.name.clone();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();
    manager
        .subscribe_event(&extension_name, "updates".into())
        .unwrap();
    manager
        .publish_event("updates".into(), serde_json::json!({"value": 7}))
        .unwrap();

    let result = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert_eq!(
        result.events,
        vec![WasiExtensionEvent {
            topic: "updates".into(),
            payload: serde_json::json!({"value": 7}),
        }]
    );
}

#[test]
fn extension_state_is_owned_by_identity() {
    let manager = WasiExtensionManager::new();
    manager
        .set_extension_state("one", serde_json::json!({"secret": 1}))
        .unwrap();
    assert_eq!(
        manager.extension_state("one"),
        Some(serde_json::json!({"secret": 1}))
    );
    assert_eq!(manager.extension_state("two"), None);
}

#[test]
fn capability_policy_rejects_undeclared_capabilities() {
    let policy = CapabilityPolicy::new(["agent"]);
    assert!(policy.allows("agent"));
    assert!(!policy.allows("tools"));
    let response = policy.denied_response("tools");
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "capability_denied");
}

#[test]
fn test_wasi_extension_manager_discovery() {
    let dir = tempdir().unwrap();
    let ext_dir = dir.path().join("extensions");
    std::fs::create_dir_all(&ext_dir).unwrap();

    let manager = WasiExtensionManager::new();
    let count = manager.discover_and_load(dir.path());
    assert_eq!(count, 0);
}

#[test]
fn v1_manifest_defaults_to_no_capabilities() {
    let manifest: WasiExtensionManifest =
        serde_json::from_str(r#"{"api_version":1,"name":"old","version":"1","description":"old"}"#)
            .unwrap();
    assert!(manifest.capabilities.is_empty());
}

#[test]
fn v2_manifest_preserves_declared_capabilities() {
    let manifest: WasiExtensionManifest = serde_json::from_str(
        r#"{"api_version":2,"name":"new","version":"1","description":"new","capabilities":["tools","agent"]}"#,
    )
    .unwrap();
    assert_eq!(manifest.capabilities, vec!["tools", "agent"]);
}

#[test]
fn load_from_bytes_rejects_unknown_manifest_api_version() {
    let result = WasiExtension::load_from_bytes(manifest_wasm(
        r#"{"api_version":99,"name":"unknown","version":"1","description":"unknown"}"#,
    ));
    let error = match result {
        Ok(_) => panic!("unknown API version was accepted"),
        Err(error) => error,
    };
    assert_eq!(error, "Unsupported extension API version: 99");
}

#[test]
fn loaded_v1_extension_has_no_capability_grants() {
    let extension = WasiExtension::load_from_bytes(manifest_wasm(
        r#"{"api_version":1,"name":"old","version":"1","description":"old","capabilities":["tools"]}"#,
    ))
    .unwrap();
    let policy = extension.capability_policy();
    assert!(!policy.allows("tools"));
    assert!(!policy.allows("agent"));
}

#[test]
fn loaded_v2_extension_has_declared_capability_grants() {
    let extension = WasiExtension::load_from_bytes(manifest_wasm(
        r#"{"api_version":2,"name":"new","version":"1","description":"new","capabilities":["tools","agent"]}"#,
    ))
    .unwrap();
    let policy = extension.capability_policy();
    assert!(policy.allows("tools"));
    assert!(policy.allows("agent"));
    assert!(!policy.allows("filesystem"));
}

#[test]
fn test_wasi_manifest_serde() {
    let manifest = WasiExtensionManifest {
        api_version: 1,
        name: "test_ext".into(),
        version: "1.0.0".into(),
        description: "Test extension".into(),
        capabilities: vec![],
        tools: vec![WasiToolDefinition {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        commands: vec![],
        hooks: vec![],
    };

    let serialized = serde_json::to_string(&manifest).unwrap();
    let deserialized: WasiExtensionManifest = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.name, "test_ext");
    assert_eq!(deserialized.tools.len(), 1);
}

#[test]
fn test_wasi_minimal_wasm_bytes() {
    let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let ext = WasiExtension::load_from_bytes(wasm_bytes).unwrap();
    assert_eq!(ext.manifest.name, "unnamed_wasi_ext");
}

#[tokio::test]
async fn structured_hook_middleware_blocks_without_message_matching() {
    let response =
        r#"{"message":"","state":{},"middleware":{"block":true,"reason":"Protected path"}}"#;
    let extension = WasiExtension::load_from_bytes(hook_wasm(2, response)).unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();
    let hook = extension_before_tool_hook_handler(
        Arc::new(tokio::sync::Mutex::new(ToolPolicy::FullAccess)),
        Arc::new(manager),
        Arc::new(CapabilityDispatcher::new()),
    );
    let result = hook(HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        tool_call_id: Some("call-1".into()),
        tool_name: Some("read_file".into()),
        tool_arguments: Some("{}".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(result.unwrap_err(), "Protected path");
}

#[tokio::test]
async fn structured_hook_v2_message_does_not_block() {
    let extension = WasiExtension::load_from_bytes(hook_wasm(
        2,
        r#"{"message":"blocked prose from v2","state":{},"middleware":{"block":false}}"#,
    ))
    .unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();
    let hook = extension_before_tool_hook_handler(
        Arc::new(tokio::sync::Mutex::new(ToolPolicy::FullAccess)),
        Arc::new(manager),
        Arc::new(CapabilityDispatcher::new()),
    );
    let result = hook(HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        tool_call_id: Some("call-1".into()),
        tool_name: Some("read_file".into()),
        tool_arguments: Some("{}".into()),
        ..Default::default()
    })
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn structured_hook_v1_message_behavior_is_preserved() {
    let extension =
        WasiExtension::load_from_bytes(hook_wasm(1, r#"{"message":"blocked by v1","state":{}}"#))
            .unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();
    let hook = extension_before_tool_hook_handler(
        Arc::new(tokio::sync::Mutex::new(ToolPolicy::FullAccess)),
        Arc::new(manager),
        Arc::new(CapabilityDispatcher::new()),
    );
    let result = hook(HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        tool_call_id: Some("call-1".into()),
        tool_name: Some("read_file".into()),
        tool_arguments: Some("{}".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(result.unwrap_err(), "blocked by v1");
}

#[test]
fn test_session_state_paths_are_isolated_and_filesystem_safe() {
    let project = tempdir().unwrap();
    let first =
        WasiExtensionManager::session_state_path(project.path(), "session/one", "example_ext");
    let second =
        WasiExtensionManager::session_state_path(project.path(), "session/two", "example_ext");

    assert_ne!(first, second);
    assert_eq!(
        first,
        project
            .path()
            .join(".threadlane/state/extensions/sessions/73657373696f6e2f6f6e65/example_ext.json")
    );
}

#[test]
fn extension_state_write_contains_unsafe_extension_name() {
    let project = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_stem = outside.path().join("escaped");
    let unsafe_name = outside_stem.to_string_lossy().into_owned();
    let outside_file = outside_stem.with_extension("json");
    let manager = WasiExtensionManager::for_project(project.path());

    manager
        .set_extension_state(&unsafe_name, serde_json::json!({"value": 7}))
        .unwrap();

    assert!(!outside_file.exists());
    let state_dir = project.path().join(".threadlane/state/extensions");
    let files = std::fs::read_dir(&state_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path().parent(), Some(state_dir.as_path()));
    assert!(files[0]
        .file_name()
        .to_string_lossy()
        .starts_with(".encoded-"));
}

#[test]
fn goal_extension_manifest_declares_tools_commands_hooks_and_capabilities() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(".threadlane/extensions/goal_ext.wasm");
    let extension = WasiExtension::load_from_file(&path).unwrap_or_else(|error| {
        panic!(
            "load deployed goal extension at {} (run scripts/build_extensions.sh): {error}",
            path.display()
        )
    });

    assert_eq!(extension.manifest.api_version, 2);
    assert_eq!(extension.manifest.name, "goal_ext");
    assert_eq!(extension.manifest.capabilities, vec!["agent", "ui"]);
    assert_eq!(
        extension
            .manifest
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["goal"]
    );
    assert_eq!(
        extension
            .manifest
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["get_goal", "update_goal", "create_goal"]
    );
    assert_eq!(extension.manifest.hooks, vec!["assistant_message"]);
}

#[test]
fn goal_extension_command_and_tool_workflow() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(".threadlane/extensions/goal_ext.wasm");
    let extension = WasiExtension::load_from_file(&path).unwrap();
    let manager = WasiExtensionManager::new();
    manager.register_extension(extension).unwrap();

    // 1. Run /goal command to activate a goal
    let cmd_res = manager
        .execute_command_with_effects("goal", "implement feature X --tokens 50k")
        .unwrap()
        .unwrap();

    assert!(cmd_res.message.contains("Goal activated"));
    assert_eq!(cmd_res.host_broker_requests.len(), 3);
    assert_eq!(cmd_res.host_broker_requests[0].request.capability, "ui");
    assert_eq!(cmd_res.host_broker_requests[1].request.capability, "ui");
    assert_eq!(cmd_res.host_broker_requests[2].request.capability, "agent");
    assert_eq!(cmd_res.host_broker_requests[2].request.operation, "request_turn");

    // 2. Call get_goal tool
    let get_res = manager
        .execute_tool_with_broker_requests("get_goal", "{}")
        .unwrap()
        .unwrap();
    assert!(get_res.response.message.unwrap().contains("implement feature X"));

    // 3. Trigger assistant_message hook -> schedules continuation turn
    let hook_args = serde_json::json!({
        "content": "Step 1 finished",
        "tool_calls": []
    });
    let hook_res = manager
        .execute_hook_with_effects("assistant_message", &hook_args.to_string())
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(hook_res.response.message.unwrap(), "continuation turn scheduled");
    assert!(hook_res.host_broker_requests.iter().any(|r| r.request.operation == "request_turn"));

    // 4. Assistant calls update_goal with status complete
    let update_args = serde_json::json!({
        "status": "complete",
        "evidence": "Tests passed"
    });
    let update_res = manager
        .execute_tool_with_broker_requests("update_goal", &update_args.to_string())
        .unwrap()
        .unwrap();
    assert!(update_res.response.message.unwrap().contains("Goal successfully marked complete"));

    // 5. Subsequent assistant_message hook recognizes goal is complete and does not request more turns
    let final_hook_res = manager
        .execute_hook_with_effects("assistant_message", &hook_args.to_string())
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(final_hook_res.response.message.unwrap(), "goal status is complete");
    assert!(!final_hook_res.host_broker_requests.iter().any(|r| r.request.operation == "request_turn"));
}
