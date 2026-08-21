use std::path::PathBuf;

use threadlane_session::{ImageAttachment, ReasoningEffort};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    AttachProject(PathBuf),
    SelectSession {
        work_dir: PathBuf,
        session_id: String,
    },
    SettleSession {
        work_dir: PathBuf,
        session_id: String,
    },
    RemoveSession {
        work_dir: PathBuf,
        session_id: String,
    },
    ToggleProject(PathBuf),
    BeginNewTask,
    SelectDraftProject(PathBuf),
    SendPrompt(String),
    SendPromptWithImages {
        text: String,
        images: Vec<ImageAttachment>,
    },
    StageBusyMessage(String),
    QueuePendingMessage,
    SteerPendingMessage,
    DismissPendingMessage,
    ToggleToolActivity(String),
    CancelGeneration,
    SelectModel(String),
    SelectReasoningEffort(ReasoningEffort),
    OpenSettings,
    CloseSettings,
    SaveOpenAiKey(String),
    SaveOpenCodeKey(String),
    SetActiveCodexAccount(String),
    RemoveCodexAccount(String),
    ToggleReasoningExpanded(String),
    OpenFileInEditor(String),
}
