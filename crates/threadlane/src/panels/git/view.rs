//! Git panel presentation and helper functions.

use crate::git::GitStatus;

pub fn git_branch_picker_labels(status: Option<&GitStatus>) -> (Vec<String>, usize) {
    let mut labels = status
        .map(|status| status.branches.clone())
        .unwrap_or_default();

    labels.retain(|label| label != "New branch…" && label != "＋ New branch…");

    let selected_branch = status.and_then(|status| status.branch.as_ref()).cloned();

    if let Some(branch) = selected_branch {
        labels.retain(|label| label != &branch);
        labels.push("New branch…".to_owned());
        labels.push(branch);
    } else if status.is_some() {
        labels.push("New branch…".to_owned());
        labels.push("detached HEAD".to_owned());
    } else {
        labels.push("Git".to_owned());
    }

    let selected = labels.len().saturating_sub(1);
    (labels, selected)
}

pub fn format_git_summary_text(
    status: &GitStatus,
    operation_pending: bool,
    commit_msg_pending: bool,
) -> String {
    let branch = status.branch.as_deref().unwrap_or("detached HEAD");
    let staged = status.files.iter().filter(|file| file.staged).count();
    let changed = status.files.iter().filter(|file| file.unstaged).count();
    let mut summary = Vec::new();
    if staged > 0 {
        summary.push(format!("{staged} staged"));
    }
    if changed > 0 {
        summary.push(format!("{changed} changed"));
    }
    if status.remote.is_some() && status.branch.is_some() && !status.has_upstream {
        summary.push("ready to publish".to_owned());
    } else if status.ahead > 0 {
        summary.push(format!("{} to push", status.ahead));
    }
    if status.behind > 0 {
        summary.push(format!("{} to pull", status.behind));
    }
    let changes = if summary.is_empty() {
        "clean".to_owned()
    } else {
        summary.join(" · ")
    };
    if operation_pending {
        format!("{branch} · working…")
    } else if commit_msg_pending {
        format!("{branch} · generating…")
    } else {
        format!("{branch} · {changes}")
    }
}
