//! Task sidebar presentation and helper functions.

use threadlane_coding_agent::TaskStatus;

#[allow(dead_code)]
pub fn format_task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Idle => "Idle",
        TaskStatus::Running => "Running",
        TaskStatus::Waiting => "Waiting",
        TaskStatus::Completed => "Completed",
        TaskStatus::Failed => "Failed",
        TaskStatus::Cancelled => "Cancelled",
        TaskStatus::Interrupted => "Interrupted",
    }
}
