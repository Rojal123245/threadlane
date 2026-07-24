use serde_json::json;
use std::collections::HashSet;
use threadlane_provider::antigravity::build_gemini_request;
use threadlane_provider::antigravity_auth::{
    build_authorization_url, generate_pkce_pair, AntigravityCredentials,
};

#[test]
fn test_pkce_generation() {
    let mut verifiers = HashSet::new();

    for _ in 0..256 {
        let (verifier, challenge) = generate_pkce_pair();
        assert!(!verifier.is_empty(), "Verifier should not be empty");
        assert!(!challenge.is_empty(), "Challenge should not be empty");
        assert_ne!(verifier, challenge, "Verifier and challenge should differ");
        assert!(
            verifiers.insert(verifier),
            "Each generated verifier should be unique"
        );
    }
}

#[test]
fn test_auth_url_construction() {
    let (_, challenge) = generate_pkce_pair();
    let state = "test_state_12345";
    let url = build_authorization_url(&challenge, state);

    assert!(
        url.contains("accounts.google.com"),
        "URL should point to Google accounts"
    );
    assert!(
        url.contains("code_challenge="),
        "URL should include PKCE code_challenge"
    );
    assert!(
        url.contains("code_challenge_method=S256"),
        "URL should use S256 PKCE method"
    );
    assert!(
        url.contains("state=test_state_12345"),
        "URL should include state parameter"
    );
    assert!(url.contains("scope="), "URL should include OAuth scopes");

    if std::env::var_os("ANTIGRAVITY_CLIENT_ID").is_none() {
        let parsed = url::Url::parse(&url).expect("authorization URL should parse");
        let client_id = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "client_id").then(|| value.into_owned()));
        assert_eq!(
            client_id.as_deref(),
            Some("1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com")
        );
    }
}

#[test]
fn test_gemini_request_building() {
    let system_prompt = "You are a helpful AI coding assistant.";
    let messages = vec![
        json!({
            "role": "user",
            "content": "Write a hello world function in Rust."
        }),
        json!({
            "role": "assistant",
            "content": "Here is a simple Hello World in Rust:\n```rust\nfn main() {\n    println!(\"Hello, world!\");\n}\n```"
        }),
    ];
    let tools = vec![json!({
        "name": "run_command",
        "description": "Execute a shell command",
        "parameters": {
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }
    })];

    let req = build_gemini_request(system_prompt, &messages, &tools);

    assert!(
        req.system_instruction.is_some(),
        "System instruction should be set"
    );
    assert_eq!(req.contents.len(), 2, "Should contain 2 messages");
    assert_eq!(req.contents[0].role, "user");
    assert_eq!(req.contents[1].role, "model");

    assert!(req.tools.is_some(), "Tools should be present");
    let decls = &req.tools.unwrap()[0].function_declarations;
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].name, "run_command");
}

#[test]
fn test_credentials_serialization() {
    let creds = AntigravityCredentials {
        access_token: "mock_access_token".to_string(),
        refresh_token: Some("mock_refresh_token".to_string()),
        expires_at: 1700000000,
        account_email: Some("developer@example.com".to_string()),
        project_id: Some("mock-gcp-project".to_string()),
    };

    let json = serde_json::to_string(&creds).expect("Serialization failed");
    let deserialized: AntigravityCredentials =
        serde_json::from_str(&json).expect("Deserialization failed");

    assert_eq!(deserialized.access_token, "mock_access_token");
    assert_eq!(
        deserialized.refresh_token.as_deref(),
        Some("mock_refresh_token")
    );
    assert_eq!(
        deserialized.account_email.as_deref(),
        Some("developer@example.com")
    );
    assert_eq!(deserialized.project_id.as_deref(), Some("mock-gcp-project"));
}
