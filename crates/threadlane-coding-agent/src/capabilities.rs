use crate::agents::{discover_agents, AgentConfig, AgentScope};
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

        let packages = project_root
            .map(|project_root| PackageManager::new().list_packages(project_root))
            .unwrap_or_default();

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
