use crate::agents::{discover_agents, AgentConfig, AgentScope};
use crate::full_trust_extension::{compute_executable_revision, TrustStore};
use crate::packages::{PackageManager, PackageRecord};
use crate::skills::{SkillManager, SkillMetadata};
use crate::wasi_extension::WasiExtensionManager;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionMetadata {
    id: String,
    package_id: Option<String>,
    name: String,
    is_full_trust: bool,
    enabled: bool,
    revision: Option<String>,
    is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCatalog {
    skills: Vec<SkillMetadata>,
    extensions: Vec<ExtensionMetadata>,
    packages: Vec<PackageRecord>,
    agents: Vec<AgentConfig>,
}

impl CapabilityCatalog {
    pub fn discover(project_root: Option<&Path>, global_dir: &Path) -> Self {
        let mut skill_mgr = SkillManager::new();
        skill_mgr.discover_skills(project_root);
        let skills = skill_mgr.list_skills();

        let pkg_mgr = PackageManager::new(global_dir.to_path_buf());
        let packages = pkg_mgr.list_packages(project_root);

        let cwd = project_root.unwrap_or(global_dir);
        let agents = discover_agents(cwd, AgentScope::Both).agents;

        let mut extensions = Vec::new();

        if let Some(proj) = project_root {
            let mut wasi_mgr = WasiExtensionManager::for_project(proj);
            wasi_mgr.discover_and_load(proj);
            for (id, ext) in wasi_mgr.get_extensions() {
                extensions.push(ExtensionMetadata {
                    id: id.clone(),
                    package_id: None,
                    name: ext.manifest.name.clone(),
                    is_full_trust: false,
                    enabled: true,
                    revision: None,
                    is_trusted: true,
                });
            }
        }

        let trust_file = global_dir.join("state/trust.json");
        let trust_store = TrustStore::load_from_file(&trust_file);

        for pkg in &packages {
            if let Some(ref exe_rel) = pkg.manifest.full_trust_executable {
                let exe_path = pkg.root_dir.join(exe_rel);
                let rev = compute_executable_revision(&exe_path).ok();
                let is_valid = rev.is_some();

                let trusted = if let Some(ref r) = rev {
                    trust_store.is_trusted(&pkg.manifest.id, r)
                } else {
                    false
                };

                extensions.push(ExtensionMetadata {
                    id: format!("{}-exe", pkg.manifest.id),
                    package_id: Some(pkg.manifest.id.clone()),
                    name: format!("{} (Executable)", pkg.manifest.name),
                    is_full_trust: true,
                    enabled: trusted && is_valid,
                    revision: rev,
                    is_trusted: trusted,
                });
            }
        }

        Self {
            skills,
            extensions,
            packages,
            agents,
        }
    }

    pub fn extensions(&self) -> &[ExtensionMetadata] {
        &self.extensions
    }

    pub fn packages(&self) -> &[PackageRecord] {
        &self.packages
    }
}

impl ExtensionMetadata {
    pub fn package_id(&self) -> Option<&str> {
        self.package_id.as_deref()
    }

    pub fn is_full_trust(&self) -> bool {
        self.is_full_trust
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    pub fn is_trusted(&self) -> bool {
        self.is_trusted
    }
}
