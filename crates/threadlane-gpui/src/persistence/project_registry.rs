use std::path::PathBuf;

pub(crate) fn global_threadlane_dir() -> PathBuf {
    threadlane_coding_agent::default_global_threadlane_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".threadlane")
    })
}

pub(crate) use threadlane_coding_agent::load_project_registry;
