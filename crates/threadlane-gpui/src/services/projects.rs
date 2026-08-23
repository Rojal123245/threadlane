//! Project-level operations used by the GPUI application.
//!
//! This module is intentionally small while the existing state methods are
//! migrated behind the application controller.

use std::path::Path;

use crate::state::{ProjectInfo, discover_sessions_in_project};

pub fn refresh_project(project: &mut ProjectInfo, work_dir: &Path) {
    project.sessions = discover_sessions_in_project(work_dir);
}
