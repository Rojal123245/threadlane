use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// A persistent subprocess managed by the host for WASI extensions.
/// Extensions reference managed processes by name across invocations.
pub(crate) struct ManagedProcess {
    pub(crate) child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    pub(crate) stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    pub(crate) stdout_buf: Arc<tokio::sync::Mutex<Vec<u8>>>,
    pub(crate) pid: u32,
    pub(crate) alive: Arc<AtomicBool>,
}

#[derive(Hash, Eq, PartialEq)]
pub(crate) struct ManagedProcessKey {
    pub(crate) extension: String,
    pub(crate) session: Option<String>,
    pub(crate) name: String,
}

pub(crate) type ManagedProcessRegistry =
    Arc<tokio::sync::Mutex<HashMap<ManagedProcessKey, ManagedProcess>>>;
