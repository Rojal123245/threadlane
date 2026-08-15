use std::path::PathBuf;

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
    StageBusyMessage(String),
    QueuePendingMessage,
    SteerPendingMessage,
    DismissPendingMessage,
    CancelGeneration,
    SelectModel(String),
    OpenSettings,
    CloseSettings,
    SaveOpenAiKey(String),
    SaveOpenCodeKey(String),
}
