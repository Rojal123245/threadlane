use crate::agents::{discover_agents, AgentConfig, AgentScope};
use crate::packages::{PackageManager, PackageRecord};
use crate::skills::{SkillManager, SkillMetadata};
use crate::wasi_extension::WasiExtensionManager;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionMetadata {
    id: String,
    name: String,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCatalog {
    skills: Vec<SkillMetadata>,
    extensions: Vec<ExtensionMetadata>,
    packages: Vec<PackageRecord>,
    agents: Vec<AgentConfig>,
}

impl CapabilityCatalog {
    pub fn discover(project_root: Option<&Path>) -> Self {
        let mut skill_mgr = SkillManager::new();
        skill_mgr.discover_skills(project_root);
        let skills = skill_mgr.list_skills();

        let packages = project_root
            .map(|project_root| PackageManager::new().list_packages(project_root))
            .unwrap_or_default();

        let agents = project_root
            .map(|project_root| discover_agents(project_root, AgentScope::Both).agents)
            .unwrap_or_default();

        let mut extensions = Vec::new();

        if let Some(proj) = project_root {
            let mut wasi_mgr = WasiExtensionManager::for_project(proj);
            wasi_mgr.discover_and_load(proj);
            for (id, ext) in wasi_mgr.get_extensions() {
                extensions.push(ExtensionMetadata {
                    id: id.clone(),
                    name: ext.manifest.name.clone(),
                    enabled: true,
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

    pub fn packages(&self) -> &[PackageRecord] {
        &self.packages
    }
}
