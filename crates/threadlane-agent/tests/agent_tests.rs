use tempfile::tempdir;
use threadlane_agent::{
    compact_messages, repair_interrupted_tool_turn, AgentMessage, AgentToolDefinition,
    CompactionOptions, ImageAttachment, SessionTree, TokenUsage,
};
use threadlane_provider::openai::{ToolCall, ToolCallFunction};

#[test]
fn test_compaction_logic() {
    let mut msgs = vec![AgentMessage::System {
        content: "System prompt".to_string(),
    }];
    for i in 0..60 {
        msgs.push(AgentMessage::User {
            content: format!("Msg {}", i),
        });
    }

    let options = CompactionOptions {
        max_messages: 20,
        preserve_recent: 5,
    };

    let compacted = compact_messages(&msgs, &options);
    assert!(compacted.len() <= 10);
    assert_eq!(compacted[0].role_str(), "system");
}

#[test]
fn test_session_tree_persistence_and_branching() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("session.jsonl");

    let mut tree = SessionTree::new("sess_1");
    tree.file_path = Some(file_path.clone());

    let n1 = tree.add_message(AgentMessage::User {
        content: "Hello".to_string(),
    });
    let _n2 = tree.add_message(AgentMessage::Assistant {
        content: Some("Hi there".to_string()),
        tool_calls: None,
        stop_reason: None,
        deferred_handle: None,
    });

    assert_eq!(tree.nodes.len(), 2);
    let loaded = SessionTree::load_from_file(&file_path).unwrap();
    assert_eq!(loaded.nodes.len(), 2);

    let forked = tree.fork_branch(&n1).unwrap();
    assert_eq!(forked.nodes.len(), 1);
    assert_eq!(forked.parent_session_id.as_deref(), Some("sess_1"));
    assert!(forked.session_id.starts_with("sess_1_fork_"));
}

#[test]
fn test_multimodal_provider_payloads() {
    use threadlane_agent::loop_engine::{convert_to_codex_llm, convert_to_llm};

    let image = ImageAttachment {
        display_name: "img1".to_string(),
        data_url: "data:image/png;base64,abc".to_string(),
    };
    let messages = vec![
        AgentMessage::System {
            content: "System".to_string(),
        },
        AgentMessage::UserWithImages {
            content: "Look at this".to_string(),
            images: vec![image],
        },
    ];

    let llm = convert_to_llm(&messages);
    assert_eq!(llm.len(), 2);
    assert_eq!(llm[1]["role"], "user");
    assert!(llm[1]["content"].is_array());

    let (instructions, codex) = convert_to_codex_llm(&messages);
    assert_eq!(instructions, "System");
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0]["role"], "user");
    assert!(codex[0]["content"].is_array());
}

#[test]
fn interrupted_tool_turn_is_removed_before_provider_replay() {
    let mut messages = vec![
        AgentMessage::System {
            content: "system".into(),
        },
        AgentMessage::User {
            content: "inspect".into(),
        },
        AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "working"}),
        },
        AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "call-a".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
                ToolCall {
                    id: "call-b".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
            ]),
            stop_reason: None,
            deferred_handle: None,
        },
        AgentMessage::Tool {
            tool_call_id: "call-a".into(),
            name: "read_file".into(),
            content: "partial".into(),
            is_error: false,
            terminate: false,
        },
    ];

    assert!(repair_interrupted_tool_turn(&mut messages));
    assert_eq!(messages.len(), 2);
    assert!(matches!(messages.last(), Some(AgentMessage::User { .. })));
}

#[test]
fn completed_tool_turn_is_preserved_for_provider_replay() {
    let mut messages = vec![
        AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-a".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        },
        AgentMessage::Tool {
            tool_call_id: "call-a".into(),
            name: "read_file".into(),
            content: "done".into(),
            is_error: false,
            terminate: false,
        },
    ];

    assert!(!repair_interrupted_tool_turn(&mut messages));
    assert_eq!(messages.len(), 2);
}

#[test]
fn test_convert_to_codex_llm_structure() {
    use threadlane_agent::loop_engine::convert_to_codex_llm;

    let messages = vec![
        AgentMessage::System {
            content: "Be helpful.".to_string(),
        },
        AgentMessage::User {
            content: "List files".to_string(),
        },
        AgentMessage::Assistant {
            content: Some("Listing files:".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: "call_abc123".to_string(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: "list_dir".to_string(),
                    arguments: "{\"path\":\".\"}".to_string(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        },
        AgentMessage::Tool {
            tool_call_id: "call_abc123".to_string(),
            name: "list_dir".to_string(),
            content: "file1.txt\nfile2.txt".to_string(),
            is_error: false,
            terminate: false,
        },
    ];

    let (instructions, items) = convert_to_codex_llm(&messages);

    assert_eq!(instructions, "Be helpful.");
    assert_eq!(items.len(), 4);

    // User message item
    assert_eq!(items[0]["type"], "message");
    assert_eq!(items[0]["role"], "user");

    // Assistant message item
    assert_eq!(items[1]["type"], "message");
    assert_eq!(items[1]["role"], "assistant");

    // Function call item
    assert_eq!(items[2]["type"], "function_call");
    assert_eq!(items[2]["call_id"], "call_abc123");
    assert_eq!(items[2]["name"], "list_dir");

    // Function call output item
    assert_eq!(items[3]["type"], "function_call_output");
    assert_eq!(items[3]["call_id"], "call_abc123");
}

fn test_definition(name: &str, description: &str) -> AgentToolDefinition {
    AgentToolDefinition::new(
        name,
        description,
        serde_json::json!({
            "type": "object",
            "properties": { "value": { "type": "string" } }
        }),
    )
}

#[test]
fn test_agent_tool_definition_provider_shapes_round_trip() {
    let mut definition = test_definition("lookup", "Looks up a value");
    definition.strict = Some(true);

    let chat = definition.to_chat_completions_tool();
    assert_eq!(chat["type"], "function");
    assert_eq!(chat["function"]["name"], "lookup");
    assert_eq!(chat["function"]["strict"], true);
    assert!(chat.get("name").is_none());

    let codex = definition.to_codex_responses_tool();
    assert_eq!(codex["type"], "function");
    assert_eq!(codex["name"], "lookup");
    assert_eq!(codex["strict"], true);
    assert!(codex.get("function").is_none());

    assert_eq!(
        AgentToolDefinition::from_provider_schema(&chat).unwrap(),
        definition
    );
    assert_eq!(
        AgentToolDefinition::from_provider_schema(&codex).unwrap(),
        definition
    );
}

#[test]
fn test_token_usage_accumulates_across_provider_turns() {
    let mut total = TokenUsage::default();
    total.accumulate(&TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_tokens: 900,
        cache_write_tokens: 0,
        total_tokens: 1020,
    });
    total.accumulate(&TokenUsage {
        input_tokens: 50,
        output_tokens: 10,
        cache_read_tokens: 1000,
        cache_write_tokens: 25,
        total_tokens: 1085,
    });

    assert_eq!(
        total,
        TokenUsage {
            input_tokens: 150,
            output_tokens: 30,
            cache_read_tokens: 1900,
            cache_write_tokens: 25,
            total_tokens: 2105,
        }
    );
}
