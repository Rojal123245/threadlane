pub mod antigravity_auth;
pub mod auth;
pub mod codex_auth;
pub mod openai_auth;
pub mod traits;

pub use antigravity_auth::*;
pub use codex_auth::*;
pub use openai_auth::*;
pub use traits::AuthProvider;

use std::sync::Arc;

/// Resolves a concrete `AuthProvider` instance by provider ID.
pub fn resolve_auth_provider(provider_id: &str) -> Option<Arc<dyn AuthProvider>> {
    match provider_id.to_lowercase().as_str() {
        "openai" => Some(Arc::new(OpenAiAuthProvider)),
        "codex" => Some(Arc::new(CodexAuthProvider)),
        "antigravity" | "google" => Some(Arc::new(AntigravityAuthProvider)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_auth_provider() {
        assert!(resolve_auth_provider("openai").is_some());
        assert_eq!(resolve_auth_provider("openai").unwrap().provider_id(), "openai");
        assert!(resolve_auth_provider("codex").is_some());
        assert_eq!(resolve_auth_provider("codex").unwrap().provider_id(), "codex");
        assert!(resolve_auth_provider("antigravity").is_some());
        assert_eq!(resolve_auth_provider("antigravity").unwrap().provider_id(), "antigravity");
        assert!(resolve_auth_provider("google").is_some());
        assert_eq!(resolve_auth_provider("google").unwrap().provider_id(), "antigravity");
        assert!(resolve_auth_provider("unknown").is_none());
    }
}
