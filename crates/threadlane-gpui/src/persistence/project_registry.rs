use std::collections::HashSet;
use std::path::PathBuf;

use crate::state::AttachedProject;

pub fn global_threadlane_dir() -> PathBuf {
    threadlane_coding_agent::default_global_threadlane_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".threadlane")
    })
}

pub fn load_project_registry() -> Vec<AttachedProject> {
    let path = global_threadlane_dir().join("gui").join("projects.json");
    let Ok(contents) = std::fs::read(&path) else {
        return Vec::new();
    };

    let Ok(projects) = serde_json::from_slice::<Vec<AttachedProject>>(&contents) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    projects
        .into_iter()
        .filter_map(|mut project| {
            project.path = std::fs::canonicalize(&project.path).unwrap_or(project.path);
            seen.insert(project.path.clone()).then_some(project)
        })
        .collect()
}

pub fn save_project_registry(projects: &[AttachedProject]) -> Result<(), String> {
    let dir = global_threadlane_dir().join("gui");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join("projects.json");
    let json = serde_json::to_string_pretty(projects).map_err(|error| error.to_string())?;
    std::fs::write(path, json).map_err(|error| error.to_string())
}
