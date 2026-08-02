use crate::traits::AuthProvider;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

const CLIENT_ID: &str = "app-8Nl2J3k7mP0xQ1vR";

fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrNumberVisitor;

    impl<'de> de::Visitor<'de> for StringOrNumberVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a number or string representing a number")
        }

        fn visit_u64<E>(self, value: u64) -> Result<u64, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<u64, E>
        where
            E: de::Error,
        {
            if value >= 0 {
                Ok(value as u64)
            } else {
                Err(de::Error::custom("expected unsigned integer"))
            }
        }

        fn visit_str<E>(self, value: &str) -> Result<u64, E>
        where
            E: de::Error,
        {
            value.parse::<u64>().map_err(de::Error::custom)
        }
    }

    deserializer.deserialize_any(StringOrNumberVisitor)
}

fn default_verification_uri() -> String {
    "https://auth.openai.com/codex/device".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_auth_id: String,
    pub user_code: String,
    #[serde(default = "default_verification_uri")]
    pub verification_uri: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(
        deserialize_with = "deserialize_string_or_number",
        default = "default_interval"
    )]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub source: String,
}

fn get_threadlane_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(home);
    path.push(".threadlane");
    let _ = fs::create_dir_all(&path);
    path
}

pub fn get_credentials_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("credentials.json");
    path
}

pub fn save_credentials(tokens: &OAuthTokens) -> Result<(), String> {
    let path = get_credentials_path();
    let creds = StoredCredentials {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        account_id: tokens.account_id.clone(),
        source: "~/.threadlane/credentials.json".to_string(),
    };
    let json = serde_json::to_string_pretty(&creds).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

pub fn is_own_source(source: &str) -> bool {
    source == "~/.threadlane/credentials.json"
}

pub fn remove_credentials() -> Result<(), String> {
    let path = get_credentials_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn get_openai_api_key_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("openai_api_key");
    path
}

fn write_secure_text_file(path: &PathBuf, contents: &str) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "Failed to store OpenAI API key".to_string())?;
    let tmp_path = parent.join(format!(
        ".openai_api_key.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Failed to store OpenAI API key".to_string())?
            .as_nanos()
    ));

    let mut options = OpenOptions::new();
    options.create_new(true).truncate(true).write(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options
        .open(&tmp_path)
        .map_err(|e| format!("Failed to store OpenAI API key: {e}"))?;

    file.write_all(contents.as_bytes())
        .map_err(|e| format!("Failed to store OpenAI API key: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("Failed to store OpenAI API key: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to store OpenAI API key: {e}"))?;
    }

    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("Failed to store OpenAI API key: {e}"))?;
    }
    fs::rename(&tmp_path, path).map_err(|e| format!("Failed to store OpenAI API key: {e}"))?;

    Ok(())
}

pub fn save_openai_api_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("OpenAI API key cannot be empty".to_string());
    }

    write_secure_text_file(&get_openai_api_key_path(), key)
}

pub fn load_openai_api_key() -> Option<String> {
    let path = get_openai_api_key_path();
    let key = fs::read_to_string(path).ok()?;
    let key = key.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

pub fn load_credentials() -> Option<StoredCredentials> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    // 1. Try ~/.threadlane/credentials.json
    let threadlane_path = get_credentials_path();
    if threadlane_path.exists() {
        if let Ok(content) = fs::read_to_string(&threadlane_path) {
            if let Ok(creds) = serde_json::from_str::<StoredCredentials>(&content) {
                if !creds.access_token.is_empty() {
                    return Some(creds);
                }
            }
        }
    }

    // 2. Try ~/.codex/auth.json
    let codex_path = PathBuf::from(&home).join(".codex").join("auth.json");
    if codex_path.exists() {
        if let Ok(content) = fs::read_to_string(&codex_path) {
            if let Ok(val) = serde_json::from_str::<Value>(&content) {
                if let Some(tokens) = val.get("tokens") {
                    if let Some(token) = tokens.get("access_token").and_then(|v| v.as_str()) {
                        let account_id = tokens
                            .get("account_id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        return Some(StoredCredentials {
                            access_token: token.to_string(),
                            refresh_token: tokens
                                .get("refresh_token")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            account_id,
                            source: "~/.codex/auth.json".to_string(),
                        });
                    }
                }
                if let Some(key) = val.get("OPENAI_API_KEY").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        return Some(StoredCredentials {
                            access_token: key.to_string(),
                            refresh_token: None,
                            account_id: None,
                            source: "~/.codex/auth.json".to_string(),
                        });
                    }
                }
            }
        }
    }

    None
}

pub async fn start_device_login() -> Result<DeviceCodeResponse, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/api/accounts/deviceauth/usercode")
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to initiate ChatGPT device login: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("Device login initiation failed ({status}): {body}"));
    }

    let text = res
        .text()
        .await
        .map_err(|e| format!("Failed to read device code body: {e}"))?;

    serde_json::from_str::<DeviceCodeResponse>(&text)
        .map_err(|e| format!("Failed to parse device code response ({e}): {text}"))
}

pub async fn poll_device_token(
    device_auth_id: &str,
    user_code: &str,
) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/api/accounts/deviceauth/token")
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "device_auth_id": device_auth_id,
            "user_code": user_code
        }))
        .send()
        .await
        .map_err(|e| format!("Error polling device token: {e}"))?;

    let body = res.text().await.unwrap_or_default();

    if body.contains("deviceauth_authorization_pending") || body.contains("authorization_pending") {
        return Err("authorization_pending".to_string());
    }

    let val: Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse OAuth response body ({e}): {body}"))?;

    if let Some(access_token) = val.get("access_token").and_then(|v| v.as_str()) {
        let tokens = OAuthTokens {
            access_token: access_token.to_string(),
            refresh_token: val
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expires_in: val.get("expires_in").and_then(|v| v.as_u64()),
            id_token: val
                .get("id_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            account_id: val
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        let _ = save_credentials(&tokens);
        return Ok(tokens);
    }

    let code_opt = val
        .get("authorization_code")
        .or_else(|| val.get("code"))
        .and_then(|v| v.as_str());

    if let Some(code) = code_opt {
        return exchange_authorization_code(code).await;
    }

    Err(format!("Unexpected OAuth token response: {body}"))
}

async fn exchange_authorization_code(code: &str) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/oauth/token")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "redirect_uri": "https://auth.openai.com/device"
        }))
        .send()
        .await
        .map_err(|e| format!("Error exchanging code for OAuth token: {e}"))?;

    let body = res.text().await.unwrap_or_default();
    let val: Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse token exchange response ({e}): {body}"))?;

    if let Some(access_token) = val.get("access_token").and_then(|v| v.as_str()) {
        let tokens = OAuthTokens {
            access_token: access_token.to_string(),
            refresh_token: val
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expires_in: val.get("expires_in").and_then(|v| v.as_u64()),
            id_token: val
                .get("id_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            account_id: val
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        let _ = save_credentials(&tokens);
        return Ok(tokens);
    }

    Err(format!("Code exchange failed: {body}"))
}

pub struct OpenAiAuthProvider;

#[async_trait::async_trait]
impl AuthProvider for OpenAiAuthProvider {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    fn has_credentials(&self) -> bool {
        load_credentials().is_some_and(|creds| is_own_source(&creds.source))
    }

    async fn get_token(&self) -> Result<String, String> {
        load_credentials()
            .filter(|creds| is_own_source(&creds.source))
            .map(|creds| creds.access_token)
            .ok_or_else(|| "No stored OpenAI credentials found. Please run /login openai".to_string())
    }

    fn clear_credentials(&self) -> Result<(), String> {
        remove_credentials()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn temp_home(name: &str) -> PathBuf {
        let mut home = std::env::temp_dir();
        home.push(format!(
            "threadlane-auth-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&home).unwrap();
        home
    }

    struct TestHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous_home: Option<OsString>,
        home: PathBuf,
    }

    impl TestHomeGuard {
        fn new(name: &str) -> Self {
            let lock = test_guard();
            let previous_home = std::env::var_os("HOME");
            let home = temp_home(name);
            std::env::set_var("HOME", &home);
            Self {
                _lock: lock,
                previous_home,
                home,
            }
        }

        fn home(&self) -> &PathBuf {
            &self.home
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            match self.previous_home.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn test_parse_device_code_response() {
        let sample_json = r#"{
            "device_auth_id": "deviceauth_123",
            "user_code": "JLHW-OEIT1",
            "interval": "5",
            "expires_at": "2026-07-21T20:56:56+00:00"
        }"#;

        let resp: DeviceCodeResponse = serde_json::from_str(sample_json).unwrap();
        assert_eq!(resp.user_code, "JLHW-OEIT1");
        assert_eq!(resp.interval, 5);
        assert_eq!(
            resp.verification_uri,
            "https://auth.openai.com/codex/device"
        );
    }

    #[test]
    fn test_openai_provider_id() {
        let openai = OpenAiAuthProvider;
        assert_eq!(openai.provider_id(), "openai");
    }

    #[test]
    fn test_save_and_load_openai_api_key_round_trip() {
        let env = TestHomeGuard::new("round-trip");

        save_openai_api_key("sk-test-123").unwrap();

        assert_eq!(load_openai_api_key().as_deref(), Some("sk-test-123"));
        assert!(env.home().join(".threadlane").join("openai_api_key").exists());
    }

    #[test]
    fn test_save_openai_api_key_rejects_empty_key() {
        let _env = TestHomeGuard::new("empty");

        let err = save_openai_api_key("   ").unwrap_err();
        assert!(err.to_lowercase().contains("empty"));
        assert!(!err.contains("sk-test-123"));
        assert!(load_openai_api_key().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_save_openai_api_key_sets_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let env = TestHomeGuard::new("perms");

        save_openai_api_key("sk-permissions").unwrap();

        let path = env.home().join(".threadlane").join("openai_api_key");
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn test_save_openai_api_key_does_not_echo_secret_on_write_error() {
        use std::os::unix::fs::PermissionsExt;

        let env = TestHomeGuard::new("write-error");

        let threadlane = env.home().join(".threadlane");
        fs::create_dir_all(&threadlane).unwrap();
        fs::set_permissions(&threadlane, fs::Permissions::from_mode(0o555)).unwrap();

        let secret = "sk-super-secret";
        let err = save_openai_api_key(secret).unwrap_err();
        assert!(!err.contains(secret));
    }

    #[test]
    fn test_load_credentials_still_reads_codex_openai_api_key() {
        let env = TestHomeGuard::new("codex");

        let codex_dir = env.home().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("auth.json"),
            r#"{"OPENAI_API_KEY":"codex-secret"}"#,
        )
        .unwrap();

        let creds = load_credentials().unwrap();
        assert_eq!(creds.access_token, "codex-secret");
        assert_eq!(creds.source, "~/.codex/auth.json");
    }
}
