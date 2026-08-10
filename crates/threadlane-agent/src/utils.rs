//! Shared agent utilities.
//!
//! Small helpers used across the agent crate that don't warrant
//! their own module.

/// A `JoinHandle` wrapper that aborts the spawned task on drop.
///
/// Used by the tool dispatcher and turn driver to ensure spawned
/// provider/tool tasks are cancelled when the parent is dropped.
pub(crate) struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    pub(crate) async fn join(mut self) -> Result<T, tokio::task::JoinError> {
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
