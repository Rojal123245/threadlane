//! Task sidebar panel state management.

#[derive(Clone, Debug, Default)]
pub struct TaskSidebarUiState {
    pub selected_task_id: Option<String>,
    pub filter_active_only: bool,
}

impl TaskSidebarUiState {
    pub fn select_task(&mut self, task_id: impl Into<String>) {
        self.selected_task_id = Some(task_id.into());
    }

    pub fn clear_selection(&mut self) {
        self.selected_task_id = None;
    }
}
