use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CLIENT_ID: &str =
    "1036056723223-m8a62495g4c1r4k5t1s5.apps.googleusercontent.com";
const DEFAULT_CLIENT_SECRET: &str = ""; // Public client PKCE
const DEFAULT_REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
const OAUTH_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64, // Unix timestamp in seconds
    pub account_email: Option<String>,
    pub project_id: Option<String>,
}

pub fn get_antigravity_credentials_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(home);
    path.push(".threadlane");
    let _ = fs::create_dir_all(&path);
    path.push("antigravity_credentials.json");
    path
}

pub fn load_antigravity_credentials() -> Option<AntigravityCredentials> {
    let path = get_antigravity_credentials_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(creds) = serde_json::from_str::<AntigravityCredentials>(&content) {
                if !creds.access_token.is_empty() {
                    return Some(creds);
                }
            }
        }
    }
    None
}

pub fn save_antigravity_credentials(creds: &AntigravityCredentials) -> Result<(), String> {
    let path = get_antigravity_credentials_path();
    let json = serde_json::to_string_pretty(creds).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

pub fn generate_pkce_pair() -> (String, String) {
    let random_bytes: Vec<u8> = (0..32).map(|_| rand_byte()).collect();
    let verifier = URL_SAFE_NO_PAD.encode(&random_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge_bytes = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(challenge_bytes);

    (verifier, challenge)
}

fn rand_byte() -> u8 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    ((now ^ (now >> 8)) & 0xFF) as u8
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn build_authorization_url(code_challenge: &str, state: &str) -> String {
    let scopes = [
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ]
    .join(" ");

    let client_id = std::env::var("ANTIGRAVITY_CLIENT_ID")
        .unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());

    let mut url = url::Url::parse(OAUTH_AUTH_URL).unwrap();
    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", DEFAULT_REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);

    url.to_string()
}

pub async fn exchange_code_for_tokens(
    code: &str,
    code_verifier: &str,
) -> Result<AntigravityCredentials, String> {
    let client_id = std::env::var("ANTIGRAVITY_CLIENT_ID")
        .unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());
    let client_secret = std::env::var("ANTIGRAVITY_CLIENT_SECRET")
        .unwrap_or_else(|_| DEFAULT_CLIENT_SECRET.to_string());

    let client = reqwest::Client::new();
    let mut params = vec![
        ("client_id", client_id.as_str()),
        ("code", code),
        ("code_verifier", code_verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", DEFAULT_REDIRECT_URI),
    ];
    if !client_secret.is_empty() {
        params.push(("client_secret", client_secret.as_str()));
    }

    let res = client
        .post(OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed to connect to Google OAuth token endpoint: {e}"))?;

    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| format!("Failed to read OAuth response body: {e}"))?;

    if !status.is_success() {
        return Err(format!("OAuth token exchange failed ({status}): {body}"));
    }

    let val: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse OAuth response JSON ({e}): {body}"))?;

    let access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing access_token in response: {body}"))?
        .to_string();

    let refresh_token = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let expires_in = val
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    let expires_at = current_timestamp() + expires_in;

    // Fetch user info email
    let account_email = fetch_user_email(&client, &access_token).await.ok();

    let creds = AntigravityCredentials {
        access_token,
        refresh_token,
        expires_at,
        account_email,
        project_id: std::env::var("ANTIGRAVITY_PROJECT_ID").ok(),
    };

    save_antigravity_credentials(&creds)?;
    Ok(creds)
}

async fn fetch_user_email(client: &reqwest::Client, access_token: &str) -> Result<String, String> {
    let res = client
        .get(USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Failed userinfo request: {e}"))?;

    if res.status().is_success() {
        let val: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        if let Some(email) = val.get("email").and_then(|v| v.as_str()) {
            return Ok(email.to_string());
        }
    }
    Err("Email not found".to_string())
}

pub async fn refresh_antigravity_token(
    creds: &AntigravityCredentials,
) -> Result<AntigravityCredentials, String> {
    let refresh_token = creds
        .refresh_token
        .as_ref()
        .ok_or_else(|| "No refresh token available".to_string())?;

    let client_id = std::env::var("ANTIGRAVITY_CLIENT_ID")
        .unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());
    let client_secret = std::env::var("ANTIGRAVITY_CLIENT_SECRET")
        .unwrap_or_else(|_| DEFAULT_CLIENT_SECRET.to_string());

    let client = reqwest::Client::new();
    let mut params = vec![
        ("client_id", client_id.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    if !client_secret.is_empty() {
        params.push(("client_secret", client_secret.as_str()));
    }

    let res = client
        .post(OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed token refresh request: {e}"))?;

    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| format!("Failed to read refresh response: {e}"))?;

    if !status.is_success() {
        return Err(format!("Token refresh failed ({status}): {body}"));
    }

    let val: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse refresh JSON ({e}): {body}"))?;

    let new_access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing access_token in refresh response: {body}"))?
        .to_string();

    let expires_in = val
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    let expires_at = current_timestamp() + expires_in;

    let new_refresh = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| creds.refresh_token.clone());

    let updated_creds = AntigravityCredentials {
        access_token: new_access_token,
        refresh_token: new_refresh,
        expires_at,
        account_email: creds.account_email.clone(),
        project_id: creds.project_id.clone(),
    };

    save_antigravity_credentials(&updated_creds)?;
    Ok(updated_creds)
}

pub async fn get_valid_antigravity_token() -> Result<String, String> {
    let creds = load_antigravity_credentials()
        .ok_or_else(|| "No stored Google Antigravity credentials found. Please run /login antigravity".to_string())?;

    let now = current_timestamp();
    // Refresh if within 5 minutes (300 seconds) of expiration
    if creds.expires_at <= now + 300 {
        if creds.refresh_token.is_some() {
            let refreshed = refresh_antigravity_token(&creds).await?;
            return Ok(refreshed.access_token);
        }
    }

    Ok(creds.access_token)
}

/// Helper function to listen locally for the OAuth callback code
pub fn listen_for_oauth_callback(
    expected_state: String,
) -> Result<tokio::sync::oneshot::Receiver<Result<String, String>>, String> {
    let listener = TcpListener::bind("127.0.0.1:51121")
        .map_err(|e| format!("Failed to bind loopback callback listener on port 51121: {e}"))?;

    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set listener non-blocking: {e}"))?;

    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let start_time = current_timestamp();
        loop {
            if current_timestamp() - start_time > 300 {
                let _ = tx.send(Err("OAuth callback timed out after 5 minutes".to_string()));
                break;
            }

            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    if let Ok(bytes_read) = stream.read(&mut buffer) {
                        let request_str = String::from_utf8_lossy(&buffer[..bytes_read]);
                        if let Some(first_line) = request_str.lines().next() {
                            if first_line.starts_with("GET /oauth-callback") {
                                let path = first_line.split_whitespace().nth(1).unwrap_or("");
                                if let Ok(parsed_url) = url::Url::parse(&format!("http://localhost:51121{path}")) {
                                    let mut code = None;
                                    let mut state = None;
                                    for (k, v) in parsed_url.query_pairs() {
                                        if k == "code" {
                                            code = Some(v.to_string());
                                        } else if k == "state" {
                                            state = Some(v.to_string());
                                        }
                                    }

                                    let html_response = if let (Some(code), Some(st)) = (code, state) {
                                        if st == expected_state {
                                            let _ = tx.send(Ok(code));
                                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body style='font-family:sans-serif;background:#0d1117;color:#58a6ff;padding:40px;text-align:center;'><h2>Google Antigravity Authentication Successful!</h2><p>You may now close this tab and return to Threadlane.</p></body></html>"
                                        } else {
                                            let _ = tx.send(Err("OAuth state mismatch".to_string()));
                                            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>State mismatch.</p></body></html>"
                                        }
                                    } else {
                                        let _ = tx.send(Err("Missing code or state in OAuth callback".to_string()));
                                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>Missing parameters.</p></body></html>"
                                    };

                                    let _ = stream.write_all(html_response.as_bytes());
                                    let _ = stream.flush();
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Error accepting callback connection: {e}")));
                    break;
                }
            }
        }
    });

    Ok(rx)
}
