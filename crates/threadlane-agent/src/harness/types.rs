use super::queue::SteerPriority;
use crate::types::{AgentMessage, ReasoningEffort, TokenUsage};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A bounded, non-secret trace label or identifier.
///
/// Trace producers must use this only for identifiers, categories, and short
/// summaries that are safe to persist. Prompt text, tool arguments/results,
/// provider response bodies, credentials, and other secret-bearing payloads
/// must never be stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TraceString(String);

impl TraceString {
    pub const MAX_BYTES: usize = 4096;

    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() > Self::MAX_BYTES {
            return Err(format!("trace string exceeds {} bytes", Self::MAX_BYTES));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TraceString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BoundedText(String);

impl BoundedText {
    pub const MAX_BYTES: usize = 32 * 1024;

    pub fn truncated(value: &str) -> Self {
        if value.len() <= Self::MAX_BYTES {
            return Self(value.into());
        }
        let mut end = Self::MAX_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        Self(value[..end].into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BoundedText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() > Self::MAX_BYTES {
            return Err(D::Error::custom("bounded text exceeds 32768 bytes"));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSnapshot {
    Full {
        /// Deliberately captured resolved system prompt. Producers must apply
        /// their configured redaction policy before constructing this variant.
        content: String,
        sha256: TraceString,
    },
    Redacted {
        sha256: TraceString,
        byte_len: usize,
        reason: TraceString,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    /// Stable capability identifiers only. Producers should cap this list at 256 items.
    pub capabilities: Vec<TraceString>,
    pub fingerprint: Option<TraceString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderOutcome {
    Completed,
    Aborted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    Authentication,
    Authorization,
    RateLimit,
    InvalidRequest,
    Unavailable,
    Timeout,
    Transport,
    Protocol,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorSummary {
    pub category: ErrorCategory,
    /// A provider-defined error code, never a response body or exception dump.
    pub code: Option<TraceString>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTraceScope {
    Once,
    Session,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTraceDecision {
    Allowed,
    Denied,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTraceSource {
    User,
    Policy,
    PersistedGrant,
    UnattendedDefault,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExecutionPhase {
    Started,
    Progress,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExecutionOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortObservation {
    SignalSent,
    ProviderNotified,
    TaskCancelled,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortInitiator {
    User,
    Timeout,
    Shutdown,
    Recovery,
    Policy,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortTarget {
    Provider,
    Tool,
    Subagent,
    Scheduler,
    ActiveRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentLifecyclePhase {
    Spawned,
    Started,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamCheckpointKind {
    AssistantText,
    Reasoning,
    ToolCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(default = "default_main_lane")]
    pub lane: String,
    pub seq: u64,
    pub timestamp: u64,
    pub message: AgentMessage,
    #[serde(default)]
    pub terminate: bool,
}

fn default_main_lane() -> String {
    "main".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub message: AgentMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEntry {
    pub(crate) id: String,
    pub(crate) run_id: Option<String>,
    pub queue: QueueKind,
    #[serde(default)]
    pub priority: Option<SteerPriority>,
    pub target: ProvisionedEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub index: usize,
    pub call_id: String,
    pub name: String,
    pub effective_args: Value,
    pub result_entry_id: String,
    pub replay: ToolReplaySafety,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationIntent {
    Run,
    Compaction,
    Navigation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueKind {
    Steer,
    FollowUp,
    NextRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolReplaySafety {
    Never,
    Safe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UsageCause {
    #[default]
    Provider,
    Discarded,
    Tool,
    Replay,
    Compaction,
    Adjustment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryState {
    pub attempt: u32,
    pub retry_at: u64,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Record {
    OperationStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        source_leaf_id: Option<String>,
        intent: OperationIntent,
    },
    AbortRequested {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
    },
    OperationFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        outcome: OperationOutcome,
        error: Option<String>,
    },
    LaneMoved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        target_leaf_id: String,
    },
    StepAttempt {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        result_entry_id: String,
        compaction_reason: Option<String>,
    },
    RetryScheduled {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        retry_at: u64,
        reason: String,
    },
    RetryConsumed {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
    },
    ToolStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        assistant_entry_id: String,
        tool_index: usize,
        tool_call_id: String,
        tool_name: String,
        effective_args: Value,
        result_entry_id: String,
        replay: ToolReplaySafety,
    },
    ToolFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        tool_call_id: String,
        result_entry_id: String,
        terminate: bool,
    },
    QueueEnqueued {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        queue: QueueKind,
        #[serde(default)]
        priority: Option<SteerPriority>,
        target: ProvisionedEntry,
    },
    QueueCancelled {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        entry_id: String,
    },
    QueueConsumed {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        entry_id: String,
    },
    WriteDeferred {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        target: ProvisionedEntry,
    },
    WriteApplied {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        entry_id: String,
    },
    FactSet {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        key: String,
        value: String,
    },
    HookResumeData {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        hook_id: String,
        data: String,
    },
    Usage {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        #[serde(default)]
        cause: UsageCause,
        #[serde(default)]
        entry_id: Option<String>,
        #[serde(default)]
        tool_call_id: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        usage: TokenUsage,
    },
    RunContextCaptured {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        model: TraceString,
        provider: TraceString,
        reasoning_effort: ReasoningEffort,
        prompt_cache_enabled: bool,
        work_dir: TraceString,
        system_prompt: PromptSnapshot,
        tool_schema_sha256: TraceString,
        enabled_tool_names: Vec<TraceString>,
        capabilities: CapabilitySnapshot,
        prompt_template_ids: Vec<TraceString>,
        git_head: Option<TraceString>,
    },
    ProviderRequestStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        provider: TraceString,
        model: TraceString,
        request_id: Option<TraceString>,
    },
    ProviderRequestFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        request_id: Option<TraceString>,
        outcome: ProviderOutcome,
        error: Option<ProviderErrorSummary>,
        duration_ms: Option<u64>,
        usage: Option<TokenUsage>,
    },
    PermissionRequested {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        attempt: Option<u32>,
        request_id: TraceString,
        capability: TraceString,
        scopes: Vec<PermissionTraceScope>,
        detail_sha256: TraceString,
        source: PermissionTraceSource,
    },
    PermissionResolved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        attempt: Option<u32>,
        request_id: TraceString,
        decision: PermissionTraceDecision,
        scope: Option<PermissionTraceScope>,
        source: PermissionTraceSource,
        remembered: bool,
    },
    ToolExecutionObserved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        tool_call_id: TraceString,
        tool_name: TraceString,
        executor_kind: TraceString,
        phase: ToolExecutionPhase,
        started_at_ms: Option<u64>,
        duration_ms: Option<u64>,
        outcome: Option<ToolExecutionOutcome>,
        exit_code: Option<i32>,
        cancelled: bool,
        is_error: Option<bool>,
        terminate: Option<bool>,
        output_sha256: Option<TraceString>,
        output_bytes: Option<u64>,
    },
    AbortObserved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        observation: AbortObservation,
        initiator: AbortInitiator,
        target: AbortTarget,
        acknowledged: bool,
        detail: Option<TraceString>,
    },
    SubagentLifecycle {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        attempt: Option<u32>,
        child_run_id: TraceString,
        parent_tool_call_id: Option<TraceString>,
        task_index: Option<u32>,
        agent_id: TraceString,
        subagent_lane: TraceString,
        phase: SubagentLifecyclePhase,
        result_entry_id: Option<TraceString>,
        error: Option<TraceString>,
    },
    StreamCheckpoint {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        request_id: TraceString,
        assistant_entry_id: Option<TraceString>,
        text: Option<BoundedText>,
        reasoning: Option<BoundedText>,
        checkpoint_index: u32,
        byte_count: u64,
        /// A non-reversible digest of the checkpoint content.
        fingerprint: TraceString,
    },
}

impl Record {
    pub(crate) fn with_seq(self, seq: u64) -> Self {
        match self {
            Self::OperationStarted {
                id,
                lane,
                timestamp,
                source_leaf_id,
                intent,
                ..
            } => Self::OperationStarted {
                id,
                seq,
                lane,
                timestamp,
                source_leaf_id,
                intent,
            },
            Self::AbortRequested {
                id,
                lane,
                timestamp,
                run_id,
                ..
            } => Self::AbortRequested {
                id,
                seq,
                lane,
                timestamp,
                run_id,
            },
            Self::OperationFinished {
                id,
                lane,
                timestamp,
                run_id,
                outcome,
                error,
                ..
            } => Self::OperationFinished {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                outcome,
                error,
            },
            Self::LaneMoved {
                id,
                lane,
                timestamp,
                run_id,
                target_leaf_id,
                ..
            } => Self::LaneMoved {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                target_leaf_id,
            },
            Self::StepAttempt {
                id,
                lane,
                timestamp,
                run_id,
                attempt,
                result_entry_id,
                compaction_reason,
                ..
            } => Self::StepAttempt {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
                result_entry_id,
                compaction_reason,
            },
            Self::RetryScheduled {
                id,
                lane,
                timestamp,
                run_id,
                attempt,
                retry_at,
                reason,
                ..
            } => Self::RetryScheduled {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
                retry_at,
                reason,
            },
            Self::RetryConsumed {
                id,
                lane,
                timestamp,
                run_id,
                attempt,
                ..
            } => Self::RetryConsumed {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
            },
            Self::ToolStarted {
                id,
                lane,
                timestamp,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay,
                ..
            } => Self::ToolStarted {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay,
            },
            Self::ToolFinished {
                id,
                lane,
                timestamp,
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
                ..
            } => Self::ToolFinished {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
            },
            Self::QueueEnqueued {
                id,
                lane,
                timestamp,
                run_id,
                queue,
                priority,
                target,
                ..
            } => Self::QueueEnqueued {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                queue,
                priority,
                target,
            },
            Self::QueueCancelled {
                id,
                lane,
                timestamp,
                run_id,
                entry_id,
                ..
            } => Self::QueueCancelled {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                entry_id,
            },
            Self::QueueConsumed {
                id,
                lane,
                timestamp,
                run_id,
                entry_id,
                ..
            } => Self::QueueConsumed {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                entry_id,
            },
            Self::WriteDeferred {
                id,
                lane,
                timestamp,
                run_id,
                target,
                ..
            } => Self::WriteDeferred {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                target,
            },
            Self::WriteApplied {
                id,
                lane,
                timestamp,
                run_id,
                entry_id,
                ..
            } => Self::WriteApplied {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                entry_id,
            },
            Self::FactSet {
                id,
                lane,
                timestamp,
                run_id,
                key,
                value,
                ..
            } => Self::FactSet {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                key,
                value,
            },
            Self::HookResumeData {
                id,
                lane,
                timestamp,
                run_id,
                hook_id,
                data,
                ..
            } => Self::HookResumeData {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                hook_id,
                data,
            },
            Self::Usage {
                id,
                lane,
                timestamp,
                run_id,
                cause,
                entry_id,
                tool_call_id,
                attempt,
                usage,
                ..
            } => Self::Usage {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                cause,
                entry_id,
                tool_call_id,
                attempt,
                usage,
            },
            mut record @ Self::RunContextCaptured { .. } => {
                if let Self::RunContextCaptured { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ProviderRequestStarted { .. } => {
                if let Self::ProviderRequestStarted { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ProviderRequestFinished { .. } => {
                if let Self::ProviderRequestFinished { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::PermissionRequested { .. } => {
                if let Self::PermissionRequested { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::PermissionResolved { .. } => {
                if let Self::PermissionResolved { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ToolExecutionObserved { .. } => {
                if let Self::ToolExecutionObserved { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::AbortObserved { .. } => {
                if let Self::AbortObserved { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::SubagentLifecycle { .. } => {
                if let Self::SubagentLifecycle { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::StreamCheckpoint { .. } => {
                if let Self::StreamCheckpoint { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::OperationStarted { id, .. }
            | Self::AbortRequested { id, .. }
            | Self::OperationFinished { id, .. }
            | Self::LaneMoved { id, .. }
            | Self::StepAttempt { id, .. }
            | Self::RetryScheduled { id, .. }
            | Self::RetryConsumed { id, .. }
            | Self::ToolStarted { id, .. }
            | Self::ToolFinished { id, .. }
            | Self::QueueEnqueued { id, .. }
            | Self::QueueCancelled { id, .. }
            | Self::QueueConsumed { id, .. }
            | Self::WriteDeferred { id, .. }
            | Self::WriteApplied { id, .. }
            | Self::FactSet { id, .. }
            | Self::HookResumeData { id, .. }
            | Self::Usage { id, .. }
            | Self::RunContextCaptured { id, .. }
            | Self::ProviderRequestStarted { id, .. }
            | Self::ProviderRequestFinished { id, .. }
            | Self::PermissionRequested { id, .. }
            | Self::PermissionResolved { id, .. }
            | Self::ToolExecutionObserved { id, .. }
            | Self::AbortObserved { id, .. }
            | Self::SubagentLifecycle { id, .. }
            | Self::StreamCheckpoint { id, .. } => id,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::OperationStarted { seq, .. }
            | Self::AbortRequested { seq, .. }
            | Self::OperationFinished { seq, .. }
            | Self::LaneMoved { seq, .. }
            | Self::StepAttempt { seq, .. }
            | Self::RetryScheduled { seq, .. }
            | Self::RetryConsumed { seq, .. }
            | Self::ToolStarted { seq, .. }
            | Self::ToolFinished { seq, .. }
            | Self::QueueEnqueued { seq, .. }
            | Self::QueueCancelled { seq, .. }
            | Self::QueueConsumed { seq, .. }
            | Self::WriteDeferred { seq, .. }
            | Self::WriteApplied { seq, .. }
            | Self::FactSet { seq, .. }
            | Self::HookResumeData { seq, .. }
            | Self::Usage { seq, .. }
            | Self::RunContextCaptured { seq, .. }
            | Self::ProviderRequestStarted { seq, .. }
            | Self::ProviderRequestFinished { seq, .. }
            | Self::PermissionRequested { seq, .. }
            | Self::PermissionResolved { seq, .. }
            | Self::ToolExecutionObserved { seq, .. }
            | Self::AbortObserved { seq, .. }
            | Self::SubagentLifecycle { seq, .. }
            | Self::StreamCheckpoint { seq, .. } => *seq,
        }
    }

    pub fn lane(&self) -> &str {
        match self {
            Self::OperationStarted { lane, .. }
            | Self::AbortRequested { lane, .. }
            | Self::OperationFinished { lane, .. }
            | Self::LaneMoved { lane, .. }
            | Self::StepAttempt { lane, .. }
            | Self::RetryScheduled { lane, .. }
            | Self::RetryConsumed { lane, .. }
            | Self::ToolStarted { lane, .. }
            | Self::ToolFinished { lane, .. }
            | Self::QueueEnqueued { lane, .. }
            | Self::QueueCancelled { lane, .. }
            | Self::QueueConsumed { lane, .. }
            | Self::WriteDeferred { lane, .. }
            | Self::WriteApplied { lane, .. }
            | Self::FactSet { lane, .. }
            | Self::HookResumeData { lane, .. }
            | Self::Usage { lane, .. }
            | Self::RunContextCaptured { lane, .. }
            | Self::ProviderRequestStarted { lane, .. }
            | Self::ProviderRequestFinished { lane, .. }
            | Self::PermissionRequested { lane, .. }
            | Self::PermissionResolved { lane, .. }
            | Self::ToolExecutionObserved { lane, .. }
            | Self::AbortObserved { lane, .. }
            | Self::SubagentLifecycle { lane, .. }
            | Self::StreamCheckpoint { lane, .. } => lane,
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::OperationStarted { id, .. } => Some(id),
            Self::AbortRequested { run_id, .. }
            | Self::OperationFinished { run_id, .. }
            | Self::LaneMoved { run_id, .. }
            | Self::StepAttempt { run_id, .. }
            | Self::RetryScheduled { run_id, .. }
            | Self::RetryConsumed { run_id, .. }
            | Self::ToolStarted { run_id, .. }
            | Self::ToolFinished { run_id, .. }
            | Self::QueueCancelled { run_id, .. }
            | Self::QueueConsumed { run_id, .. }
            | Self::WriteDeferred { run_id, .. }
            | Self::WriteApplied { run_id, .. } => Some(run_id),
            Self::RunContextCaptured { run_id, .. }
            | Self::ProviderRequestStarted { run_id, .. }
            | Self::ProviderRequestFinished { run_id, .. }
            | Self::ToolExecutionObserved { run_id, .. }
            | Self::AbortObserved { run_id, .. }
            | Self::StreamCheckpoint { run_id, .. } => Some(run_id),
            Self::FactSet { run_id, .. }
            | Self::HookResumeData { run_id, .. }
            | Self::QueueEnqueued { run_id, .. }
            | Self::Usage { run_id, .. }
            | Self::PermissionRequested { run_id, .. }
            | Self::PermissionResolved { run_id, .. }
            | Self::SubagentLifecycle { run_id, .. } => run_id.as_deref(),
        }
    }

    pub fn turn(&self) -> Option<u32> {
        match self {
            Self::StepAttempt { attempt, .. }
            | Self::RetryScheduled { attempt, .. }
            | Self::RetryConsumed { attempt, .. } => Some(*attempt),
            Self::Usage { attempt, .. }
            | Self::RunContextCaptured { attempt, .. }
            | Self::PermissionRequested { attempt, .. }
            | Self::PermissionResolved { attempt, .. }
            | Self::ToolExecutionObserved { attempt, .. }
            | Self::AbortObserved { attempt, .. }
            | Self::SubagentLifecycle { attempt, .. }
            | Self::StreamCheckpoint { attempt, .. } => *attempt,
            Self::ProviderRequestStarted { attempt, .. }
            | Self::ProviderRequestFinished { attempt, .. } => Some(*attempt),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneStatus {
    Idle,
    SuspendedCrash,
    SuspendedDeferred,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolState {
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_index: usize,
    pub tool_call_id: String,
    pub tool_name: String,
    pub result_entry_id: String,
    pub replay: ToolReplaySafety,
    pub completed: bool,
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneState {
    pub name: String,
    pub status: LaneStatus,
    pub leaf_id: Option<String>,
    pub open_operation: Option<String>,
    pub attempts: u32,
    #[serde(default)]
    pub retry: Option<RetryState>,
    pub queued: Vec<QueuedEntry>,
    pub deferred_writes: Vec<ProvisionedEntry>,
    pub abort_requested: bool,
    pub usage: TokenUsage,
    pub tools: Vec<ToolState>,
    #[serde(default)]
    pub facts: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) resume_data: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReducedState {
    pub lanes: Vec<LaneState>,
}

impl ReducedState {
    pub fn lane(&self, name: &str) -> Option<&LaneState> {
        self.lanes.iter().find(|lane| lane.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReduceError {
    DuplicateId(String),
    NonMonotonicSequence { previous: u64, current: u64 },
    MissingParent(String),
    InvalidLane(String),
    MultipleOpenOperations(String),
    UnknownOperation(String),
    InvalidRecord(String),
    Storage(String),
}

impl std::fmt::Display for ReduceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReduceError {}

#[derive(Debug, Clone, Default)]
pub struct RecoveryResult {
    pub recovered_open_operations: usize,
    pub open_operation_ids: Vec<String>,
    pub abort_requested_operation_ids: Vec<String>,
    pub unreplayable_tools: usize,
    pub safe_tools_to_replay: Vec<Record>,
}

#[derive(Debug, Clone)]
pub struct InterruptedSubagentLane {
    pub lane: String,
    pub run_id: String,
    pub source_leaf_id: Option<String>,
    pub started_seq: u64,
    pub task: String,
    pub task_attempted: bool,
    pub messages: Vec<AgentMessage>,
    pub safe_tools: Vec<Record>,
    pub unsafe_tools: Vec<Record>,
}
