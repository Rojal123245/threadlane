//! Git panel presentation and helper functions.

use crate::git::GitStatus;

pub fn format_git_branch_label(status: &GitStatus) -> String {
    if let Some(branch) = &status.branch {
        if status.detached {
            format!("detached @ {}", branch)
        } else {
            branch.clone()
        }
    } else {
        "No branch".to_string()
    }
}
