use std::sync::mpsc::Sender;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub enum ProviderAuthEvent {
    Status(String),
    Connected(String),
    Error(String),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    static EXECUTOR: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    EXECUTOR
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("threadlane-gpui-auth")
                .build()
                .map_err(|error| format!("Failed to start authentication runtime: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) fn start_chatgpt_login(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let (verifier, challenge) = threadlane_auth::antigravity_auth::generate_pkce_pair();
    let (state, _) = threadlane_auth::antigravity_auth::generate_pkce_pair();
    let authorization_url =
        threadlane_auth::openai_auth::build_browser_oauth_url(&challenge, &state);

    robius_open::Uri::new(&authorization_url)
        .open()
        .map_err(|error| format!("Failed to open ChatGPT sign-in: {error:?}"))?;

    let _ = tx.send(ProviderAuthEvent::Status(
        "Finish signing in to ChatGPT in your browser (select Personal Workspace if prompted)."
            .to_string(),
    ));

    executor()?.spawn(async move {
        let result = async {
            let code =
                threadlane_auth::openai_auth::listen_for_browser_oauth_callback(state).await?;
            threadlane_auth::openai_auth::exchange_browser_code_for_tokens(&code, &verifier).await
        }
        .await;

        match result {
            Ok(account) => {
                let _ = tx.send(ProviderAuthEvent::Connected(format!(
                    "Connected ChatGPT account ({}).",
                    account.label
                )));
            }
            Err(error) => {
                let _ = tx.send(ProviderAuthEvent::Error(format!(
                    "ChatGPT sign-in failed: {error}"
                )));
            }
        }
    });
    Ok(())
}

pub(crate) fn start_antigravity_login(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let (verifier, challenge) = threadlane_provider::antigravity_auth::generate_pkce_pair();
    let (state, _) = threadlane_provider::antigravity_auth::generate_pkce_pair();
    let authorization_url =
        threadlane_provider::antigravity_auth::build_authorization_url(&challenge, &state);
    robius_open::Uri::new(&authorization_url)
        .open()
        .map_err(|error| format!("Failed to open Google sign-in: {error:?}"))?;

    let _ = tx.send(ProviderAuthEvent::Status(
        "Finish Google Antigravity sign-in in your browser.".to_string(),
    ));
    executor()?.spawn(async move {
        let result = async {
            let code =
                threadlane_provider::antigravity_auth::listen_for_oauth_callback(state).await?;
            threadlane_provider::antigravity_auth::exchange_code_for_tokens(&code, &verifier).await
        }
        .await;

        match result {
            Ok(credentials) => {
                let account = credentials
                    .account_email
                    .unwrap_or_else(|| "Google account".to_string());
                let _ = tx.send(ProviderAuthEvent::Connected(format!(
                    "Connected Google Antigravity as {account}."
                )));
            }
            Err(error) => {
                let _ = tx.send(ProviderAuthEvent::Error(format!(
                    "Google Antigravity sign-in failed: {error}"
                )));
            }
        }
    });
    Ok(())
}

pub(crate) fn connect_github_cli(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    match threadlane_auth::github_auth::sync_from_gh_cli() {
        Ok(creds) => {
            let label = creds.username.unwrap_or_else(|| "GitHub user".to_string());
            let _ = tx.send(ProviderAuthEvent::Connected(format!(
                "Connected GitHub via CLI as @{label}."
            )));
            Ok(())
        }
        Err(err) => {
            let _ = tx.send(ProviderAuthEvent::Error(format!(
                "Failed to connect GitHub CLI: {err}"
            )));
            Err(err)
        }
    }
}

pub(crate) fn save_github_pat(token: &str, tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    match threadlane_auth::github_auth::save_github_token(token, None, "pat") {
        Ok(_) => {
            let _ = tx.send(ProviderAuthEvent::Connected(
                "Connected GitHub using Personal Access Token.".to_string(),
            ));
            Ok(())
        }
        Err(err) => {
            let _ = tx.send(ProviderAuthEvent::Error(format!(
                "Failed to save GitHub token: {err}"
            )));
            Err(err)
        }
    }
}

pub(crate) fn disconnect_github() -> Result<(), String> {
    threadlane_auth::github_auth::remove_github_credentials()
}

pub(crate) fn disconnect_gitlab() -> Result<(), String> {
    threadlane_auth::github_auth::remove_gitlab_credentials()
}
