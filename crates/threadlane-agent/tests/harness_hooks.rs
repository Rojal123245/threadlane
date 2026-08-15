use std::sync::Arc;
use threadlane_agent::harness::{
    AgentHarness, HookContext, HookEffect, HookKind, HookRegistry, MemoryStore,
};

#[tokio::test]
async fn hooks_run_in_registration_order_and_before_tool_fails_closed() {
    let hooks = HookRegistry::default();
    hooks
        .register(
            HookKind::BeforeTool,
            "first",
            Arc::new(|_| Box::pin(async { Err("blocked".into()) })),
        )
        .unwrap();
    hooks
        .register(
            HookKind::BeforeTool,
            "second",
            Arc::new(|_| Box::pin(async { Err("also blocked".into()) })),
        )
        .unwrap();
    let context = HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        run_id: Some("r".into()),
        ..Default::default()
    };
    let failures = hooks.run_before_tool(&context).await.unwrap_err();
    assert_eq!(
        failures
            .iter()
            .map(|failure| failure.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[tokio::test]
async fn after_tool_hooks_preserve_result_effects() {
    let hooks = HookRegistry::default();
    hooks
        .register(
            HookKind::AfterTool,
            "terminate",
            Arc::new(|_| {
                Box::pin(async {
                    Ok(HookEffect {
                        terminate: Some(true),
                        ..Default::default()
                    })
                })
            }),
        )
        .unwrap();

    let run = hooks
        .run_after_tool(&HookContext {
            session_id: "s".into(),
            lane: "main".into(),
            ..Default::default()
        })
        .await;

    assert!(run.failures.is_empty());
    assert_eq!(run.effect.terminate, Some(true));
}

#[tokio::test]
async fn resume_data_is_scoped_to_the_matching_stable_hook_id() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hooks = HookRegistry::default();
    let first_seen = seen.clone();
    hooks
        .register(
            HookKind::BeforeResume,
            "first",
            Arc::new(move |context| {
                let first_seen = first_seen.clone();
                Box::pin(async move {
                    first_seen.lock().unwrap().push(context.resume_data.clone());
                    Ok(HookEffect::default())
                })
            }),
        )
        .unwrap();
    let second_seen = seen.clone();
    hooks
        .register(
            HookKind::BeforeResume,
            "second",
            Arc::new(move |context| {
                let second_seen = second_seen.clone();
                Box::pin(async move {
                    second_seen
                        .lock()
                        .unwrap()
                        .push(context.resume_data.clone());
                    Ok(HookEffect::default())
                })
            }),
        )
        .unwrap();
    hooks.set_resume_data("second", "checkpoint-2").unwrap();

    let context = HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        run_id: Some("r".into()),
        ..Default::default()
    };
    assert!(hooks.run_before_resume(&context).await.is_empty());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None, Some("checkpoint-2".into())]
    );
}

#[tokio::test]
async fn resume_data_round_trips_through_the_durable_harness() {
    let mut harness = AgentHarness::new(MemoryStore::new("s"));
    harness
        .set_hook_resume_data("main", "checkpoint", "saved", Some("run-1".into()))
        .unwrap();
    harness.drive_to_completion().unwrap();

    let seen = Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    harness
        .hooks_mut()
        .register(
            HookKind::BeforeResume,
            "checkpoint",
            Arc::new(move |context| {
                let captured = captured.clone();
                Box::pin(async move {
                    *captured.lock().unwrap() = context.resume_data.clone();
                    Ok(HookEffect::default())
                })
            }),
        )
        .unwrap();
    harness.restore_hooks_for_lane("main").unwrap();
    let context = HookContext {
        session_id: "s".into(),
        lane: "main".into(),
        run_id: Some("run-1".into()),
        ..Default::default()
    };
    assert!(harness.hooks().run_before_resume(&context).await.is_empty());
    assert_eq!(*seen.lock().unwrap(), Some("saved".into()));
}
