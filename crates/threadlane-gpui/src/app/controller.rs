use super::actions::AppAction;
use crate::state::AppState;

/// Application intent boundary used by screens.
///
/// The controller is intentionally small for now. Keeping actions in one place
/// gives us a stable seam for moving backend work out of `AppState` incrementally.
pub fn dispatch(state: &mut AppState, action: AppAction) {
    match action {
        AppAction::AttachProject(path) => {
            let _ = state.attach_project(path);
        }
        AppAction::SelectSession {
            work_dir,
            session_id,
        } => {
            state.select_session(work_dir, session_id);
        }
        AppAction::SettleSession {
            work_dir,
            session_id,
        } => {
            let _ = state.settle_session(work_dir, session_id);
        }
        AppAction::RemoveSession {
            work_dir,
            session_id,
        } => {
            let _ = state.remove_session(work_dir, session_id);
        }
        AppAction::ToggleProject(path) => state.toggle_project_expanded(&path),
        AppAction::CreateSession => {
            let _ = state.create_new_session();
        }
        AppAction::SendPrompt(text) => {
            let _ = state.send_prompt(text);
        }
        AppAction::SelectModel(model) => state.set_selected_model(model),
        AppAction::ToggleSettings => state.toggle_settings_modal(),
        AppAction::SaveOpenAiKey(key) => {
            let _ = state.save_openai_key(key);
        }
        AppAction::SaveOpenCodeKey(key) => {
            let _ = state.save_opencode_key(key);
        }
    }
}
