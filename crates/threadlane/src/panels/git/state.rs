//! Git panel state management.

use crate::git::GitStatus;
use std::collections::HashSet;

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct GitUiState {
    pub status: GitStatus,
    pub active_diff_file: Option<String>,
    pub selected_files: HashSet<String>,
    pub feedback_error: Option<String>,
    pub feedback_success: Option<String>,
    pub is_new_branch_input_visible: bool,
    pub is_pr_modal_visible: bool,
    pub is_generating_commit_msg: bool,
}

#[allow(dead_code)]
impl GitUiState {
    pub fn clear_feedback(&mut self) {
        self.feedback_error = None;
        self.feedback_success = None;
    }

    pub fn set_error(&mut self, err: impl Into<String>) {
        self.feedback_error = Some(err.into());
        self.feedback_success = None;
    }

    pub fn set_success(&mut self, msg: impl Into<String>) {
        self.feedback_success = Some(msg.into());
        self.feedback_error = None;
    }
}
