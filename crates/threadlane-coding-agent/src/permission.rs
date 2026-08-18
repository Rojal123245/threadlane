use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use threadlane_agent::{AgentEvent, PermissionRequest, PermissionScope};
use tokio::sync::oneshot;

const PERMISSIONS_FILE: &str = ".threadlane/permissions.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Clone)]
pub struct PermissionHandle {
    inner: Arc<PermissionManagerInner>,
}

pub(crate) struct PermissionManager {
    handle: PermissionHandle,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
}

struct PermissionManagerInner {
    interactive: AtomicBool,
    next_id: AtomicU64,
    pending: Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
    project_root: PathBuf,
    persistent: Mutex<PersistentPermissions>,
}

#[derive(Default, Deserialize, Serialize)]
struct PersistentPermissions {
    #[serde(default)]
    network_hosts: HashSet<String>,
}

impl PermissionManager {
    pub(crate) fn new(
        project_root: PathBuf,
        event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    ) -> Self {
        let persistent = load_permissions(&project_root);
        Self {
            handle: PermissionHandle {
                inner: Arc::new(PermissionManagerInner {
                    interactive: AtomicBool::new(false),
                    next_id: AtomicU64::new(1),
                    pending: Mutex::new(HashMap::new()),
                    project_root,
                    persistent: Mutex::new(persistent),
                }),
            },
            event_tx,
        }
    }

    pub(crate) fn handle(&self) -> PermissionHandle {
        self.handle.clone()
    }

    pub(crate) fn network_host_is_approved(&self, host: &str) -> bool {
        self.handle
            .inner
            .persistent
            .lock()
            .is_ok_and(|permissions| permissions.network_hosts.contains(host))
    }

    pub(crate) async fn request_network_host(&self, host: &str, url: &str) -> PermissionDecision {
        if !self.handle.inner.interactive.load(Ordering::SeqCst) {
            return PermissionDecision::Deny;
        }
        let id = format!(
            "permission-{}",
            self.handle.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.handle.inner.pending.lock() {
            pending.insert(id.clone(), tx);
        } else {
            return PermissionDecision::Deny;
        }
        let request = PermissionRequest {
            id: id.clone(),
            capability: "network".into(),
            title: format!("Connect to {host}"),
            detail: url.to_owned(),
            scopes: vec![PermissionScope::Once, PermissionScope::Always],
        };
        if self
            .event_tx
            .send(AgentEvent::PermissionRequested { request })
            .is_err()
        {
            self.handle.remove_pending(&id);
            return PermissionDecision::Deny;
        }
        let guard = PendingRequestGuard {
            handle: self.handle.clone(),
            request_id: id,
        };
        let decision = rx.await.unwrap_or(PermissionDecision::Deny);
        drop(guard);
        if decision == PermissionDecision::AllowAlways && self.persist_network_host(host).is_err() {
            return PermissionDecision::Deny;
        }
        decision
    }

    fn persist_network_host(&self, host: &str) -> Result<(), String> {
        let mut permissions = self
            .handle
            .inner
            .persistent
            .lock()
            .map_err(|_| "permission settings are unavailable".to_string())?;
        permissions.network_hosts.insert(host.to_owned());
        save_permissions(&self.handle.inner.project_root, &permissions)
    }
}

struct PendingRequestGuard {
    handle: PermissionHandle,
    request_id: String,
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        self.handle.remove_pending(&self.request_id);
    }
}

impl PermissionHandle {
    pub fn set_interactive(&self, interactive: bool) {
        self.inner.interactive.store(interactive, Ordering::SeqCst);
    }

    pub fn resolve(&self, request_id: &str, decision: PermissionDecision) -> bool {
        self.inner
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(request_id))
            .is_some_and(|sender| sender.send(decision).is_ok())
    }

    fn remove_pending(&self, request_id: &str) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.remove(request_id);
        }
    }
}

fn load_permissions(project_root: &Path) -> PersistentPermissions {
    fs::read(project_root.join(PERMISSIONS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_permissions(
    project_root: &Path,
    permissions: &PersistentPermissions,
) -> Result<(), String> {
    let path = project_root.join(PERMISSIONS_FILE);
    let parent = path
        .parent()
        .ok_or_else(|| "permission settings path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(permissions).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn unattended_requests_default_to_deny() {
        let dir = tempdir().unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let manager = PermissionManager::new(dir.path().to_path_buf(), event_tx);

        assert_eq!(
            manager
                .request_network_host("example.com", "https://example.com")
                .await,
            PermissionDecision::Deny
        );
    }

    #[tokio::test]
    async fn allow_once_does_not_persist_host() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let manager = Arc::new(PermissionManager::new(dir.path().to_path_buf(), event_tx));
        let handle = manager.handle();
        handle.set_interactive(true);
        let request_manager = manager.clone();
        let task = tokio::spawn(async move {
            request_manager
                .request_network_host("example.com", "https://example.com/page")
                .await
        });
        let AgentEvent::PermissionRequested { request } = events.recv().await.unwrap() else {
            panic!("expected permission request");
        };
        assert!(handle.resolve(&request.id, PermissionDecision::AllowOnce));
        assert_eq!(task.await.unwrap(), PermissionDecision::AllowOnce);
        assert!(!manager.network_host_is_approved("example.com"));
    }

    #[tokio::test]
    async fn always_allow_persists_exact_host() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let manager = Arc::new(PermissionManager::new(dir.path().to_path_buf(), event_tx));
        let handle = manager.handle();
        handle.set_interactive(true);
        let request_manager = manager.clone();
        let task = tokio::spawn(async move {
            request_manager
                .request_network_host("example.com", "https://example.com/page")
                .await
        });
        let AgentEvent::PermissionRequested { request } = events.recv().await.unwrap() else {
            panic!("expected permission request");
        };
        assert!(handle.resolve(&request.id, PermissionDecision::AllowAlways));
        assert_eq!(task.await.unwrap(), PermissionDecision::AllowAlways);
        assert!(manager.network_host_is_approved("example.com"));
        assert!(!manager.network_host_is_approved("sub.example.com"));

        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let restored = PermissionManager::new(dir.path().to_path_buf(), event_tx);
        assert!(restored.network_host_is_approved("example.com"));
    }
}
