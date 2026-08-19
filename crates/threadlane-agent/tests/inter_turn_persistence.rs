use std::sync::{Arc, Mutex};

use threadlane_agent::{AgentMessage, AssistantMessageRecorder};

fn recorder(
    events: Arc<Mutex<Vec<String>>>,
    fail_on: Option<String>,
) -> AssistantMessageRecorder {
    Arc::new(move |message| {
        let events = events.clone();
        let fail_on = fail_on.clone();
        Box::pin(async move {
            let content = match message {
                AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                    content
                }
                _ => "other".into(),
            };
            events.lock().unwrap().push(content.clone());
            if fail_on.as_deref() == Some(content.as_str()) {
                Err("journal unavailable".into())
            } else {
                Ok(())
            }
        })
    })
}

#[tokio::test]
async fn inter_turn_messages_are_persisted_in_provider_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorder = recorder(events.clone(), None);
    let messages = vec![
        AgentMessage::user("steer", vec![]),
        AgentMessage::user("follow-up", vec![]),
    ];

    for message in &messages {
        recorder(message.clone()).await.unwrap();
    }

    assert_eq!(&*events.lock().unwrap(), &["steer", "follow-up"]);
}

#[tokio::test]
async fn inter_turn_persistence_stops_at_first_failure() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorder = recorder(events.clone(), Some("fail".into()));
    let messages = vec![
        AgentMessage::user("first", vec![]),
        AgentMessage::user("fail", vec![]),
        AgentMessage::user("must-not-run", vec![]),
    ];

    let mut result = Ok(());
    for message in &messages {
        result = recorder(message.clone()).await;
        if result.is_err() {
            break;
        }
    }

    assert_eq!(result.unwrap_err(), "journal unavailable");
    assert_eq!(&*events.lock().unwrap(), &["first", "fail"]);
}

#[tokio::test]
async fn tool_results_are_persisted_before_continuation_state() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorder: AssistantMessageRecorder = {
        let events = events.clone();
        Arc::new(move |message| {
            let events = events.clone();
            Box::pin(async move {
                if let AgentMessage::Tool { tool_call_id, .. } = message {
                    events.lock().unwrap().push(tool_call_id);
                }
                Ok(())
            })
        })
    };
    let messages = vec![
        AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read".into(),
            content: "one".into(),
            is_error: false,
            terminate: false,
        },
        AgentMessage::Tool {
            tool_call_id: "call-2".into(),
            name: "read".into(),
            content: "two".into(),
            is_error: false,
            terminate: false,
        },
    ];

    for message in &messages {
        recorder(message.clone()).await.unwrap();
    }

    assert_eq!(&*events.lock().unwrap(), &["call-1", "call-2"]);
}
