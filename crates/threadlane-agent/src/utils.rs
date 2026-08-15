use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A `JoinHandle` wrapper that aborts the spawned task on drop.
///
/// Used by the tool dispatcher and turn driver to ensure spawned
/// provider/tool tasks are cancelled when the parent is dropped.
pub struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    pub fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    pub async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        let result = self.handle.as_mut().expect("task handle missing").await;
        self.handle = None;
        result
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Consolidates home directory resolution across crates.
pub fn dirs_home() -> Option<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

/// Returns the current Unix timestamp in milliseconds.
pub fn now_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns the current Unix timestamp in seconds.
pub fn now_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
