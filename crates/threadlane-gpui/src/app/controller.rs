use super::actions::AppAction;
use crate::state::AppState;

/// Application intent boundary used by screens.
///
/// The controller is intentionally small for now. Keeping actions in one place
/// gives us a stable seam for moving backend work out of `AppState` incrementally.
pub(crate) fn dispatch(state: &mut AppState, action: AppAction) {
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
            if let Err(error) = state.settle_session(work_dir, session_id) {
                state.session_status = Some(error);
            }
        }
        AppAction::RemoveSession {
            work_dir,
            session_id,
        } => {
            if let Err(error) = state.remove_session(work_dir, session_id) {
                state.session_status = Some(error);
            }
        }
        AppAction::ToggleProject(path) => state.toggle_project_expanded(&path),
        AppAction::BeginNewTask => state.begin_new_task(),
        AppAction::SelectDraftProject(path) => state.select_draft_project(path),
        AppAction::SendPrompt(text) => {
            let _ = state.send_prompt(text);
        }
        AppAction::SendPromptWithImages { text, images } => {
            let _ = state.send_prompt_with_images(text, images);
        }
        AppAction::StageBusyMessage(text) => {
            let _ = state.stage_busy_message(text);
        }
        AppAction::QueuePendingMessage => {
            let _ = state.queue_pending_message();
        }
        AppAction::SteerPendingMessage => {
            let _ = state.steer_pending_message();
        }
        AppAction::DismissPendingMessage => state.dismiss_pending_message(),
        AppAction::ToggleToolActivity(tool_call_id) => state.toggle_tool_activity(&tool_call_id),
        AppAction::AcceptEditProposal(proposal_id) => {
            if let Err(error) = state.accept_edit_proposal(&proposal_id) {
                state.session_status = Some(error);
            }
        }
        AppAction::CancelGeneration => {
            let _ = state.cancel_generation();
        }
        AppAction::SelectModel(model) => state.set_selected_model(model),
        AppAction::SelectReasoningEffort(effort) => state.set_reasoning_effort(effort),
        AppAction::OpenSettings => state.open_settings(),
        AppAction::CloseSettings => state.close_settings(),
        AppAction::SaveOpenAiKey(key) => {
            let _ = state.save_openai_key(key);
        }
        AppAction::SaveOpenCodeKey(key) => {
            let _ = state.save_opencode_key(key);
        }
        AppAction::ToggleReasoningExpanded(msg_id) => {
            if let Some(message) = state.messages.iter_mut().find(|m| m.id == msg_id) {
                message.reasoning_expanded = !message.reasoning_expanded;
            }
        }
        AppAction::OpenFileInEditor(path) => state.request_open_file(path),
    }
}
