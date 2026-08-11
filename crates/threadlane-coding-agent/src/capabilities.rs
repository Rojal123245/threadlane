use threadlane_wasi::packages::{default_global_threadlane_dir, ExtensionManager, ExtensionRecord};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct CapabilityCatalog {
    extensions: Vec<ExtensionRecord>,
}

impl CapabilityCatalog {
    pub fn discover(project_root: Option<&Path>) -> Self {
        let global_threadlane_dir = default_global_threadlane_dir();
        Self::discover_with_roots(global_threadlane_dir.as_deref(), project_root)
    }

    pub fn discover_with_roots(
        global_threadlane_dir: Option<&Path>,
        project_root: Option<&Path>,
    ) -> Self {
        let extensions = ExtensionManager::new(
            global_threadlane_dir.map(Path::to_path_buf),
            project_root.map(Path::to_path_buf),
        )
        .discover();

        Self { extensions }
    }

    pub fn extensions(&self) -> &[ExtensionRecord] {
        &self.extensions
    }
}
