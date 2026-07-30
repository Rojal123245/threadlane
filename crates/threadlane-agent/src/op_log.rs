use crate::session_tree::{session_file_lock, SessionTree};
use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolReplaySafety {
    Never,
    Safe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueKind {
    Steer,
    FollowUp,
    NextRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpRecord {
    OperationStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        #[serde(default)]
        source_leaf_id: Option<String>,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt_override: Option<String>,
    },
    TaskAttempt {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        task: String,
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
        effective_args: serde_json::Value,
        result_entry_id: String,
        replay: ToolReplaySafety,
    },
    QueueEnqueued {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        queue: QueueKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<SteerPriority>,
        target: AgentMessage,
    },
    WriteDeferred {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        target: AgentMessage,
    },
    Navigation {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        target_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_entry_id: Option<String>,
    },
    OperationFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        outcome: OpOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl OpRecord {
    pub fn id(&self) -> &str {
        match self {
            Self::OperationStarted { id, .. }
            | Self::TaskAttempt { id, .. }
            | Self::ToolStarted { id, .. }
            | Self::QueueEnqueued { id, .. }
            | Self::WriteDeferred { id, .. }
            | Self::Navigation { id, .. }
            | Self::OperationFinished { id, .. } => id,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::OperationStarted { seq, .. }
            | Self::TaskAttempt { seq, .. }
            | Self::ToolStarted { seq, .. }
            | Self::QueueEnqueued { seq, .. }
            | Self::WriteDeferred { seq, .. }
            | Self::Navigation { seq, .. }
            | Self::OperationFinished { seq, .. } => *seq,
        }
    }

    pub fn lane(&self) -> &str {
        match self {
            Self::OperationStarted { lane, .. }
            | Self::TaskAttempt { lane, .. }
            | Self::ToolStarted { lane, .. }
            | Self::QueueEnqueued { lane, .. }
            | Self::WriteDeferred { lane, .. }
            | Self::Navigation { lane, .. }
            | Self::OperationFinished { lane, .. } => lane,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SteerPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerItem {
    pub message: AgentMessage,
    pub priority: SteerPriority,
    pub timestamp_ms: u128,
}

impl PartialEq for SteerItem {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.timestamp_ms == other.timestamp_ms
    }
}

impl Eq for SteerItem {}

impl PartialOrd for SteerItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SteerItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.priority.cmp(&other.priority).reverse() {
            std::cmp::Ordering::Equal => self.timestamp_ms.cmp(&other.timestamp_ms),
            ord => ord,
        }
    }
}

/// A per-lane in-memory message queue supporting steering and follow-ups.
#[derive(Debug, Clone, Default)]
pub struct LaneQueue {
    pub steer: Vec<SteerItem>,
    pub follow_up: VecDeque<AgentMessage>,
    pub next_run: VecDeque<AgentMessage>,
}

impl LaneQueue {
    pub fn enqueue(&mut self, kind: QueueKind, message: AgentMessage) {
        match kind {
            QueueKind::Steer => self.enqueue_steer_with_priority(message, SteerPriority::Normal),
            QueueKind::FollowUp => self.follow_up.push_back(message),
            QueueKind::NextRun => self.next_run.push_back(message),
        }
    }

    pub fn enqueue_steer_with_priority(&mut self, message: AgentMessage, priority: SteerPriority) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.steer.push(SteerItem {
            message,
            priority,
            timestamp_ms,
        });
        self.steer.sort();
    }

    pub fn pop_steer(&mut self) -> Option<AgentMessage> {
        if self.steer.is_empty() {
            None
        } else {
            Some(self.steer.remove(0).message)
        }
    }

    pub fn pop_follow_up(&mut self) -> Option<AgentMessage> {
        self.follow_up.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.steer.is_empty() && self.follow_up.is_empty() && self.next_run.is_empty()
    }
}

/// Appends a single operation record to a sidecar `.oplog.jsonl` file under session file lock.
pub fn append_op_record_to_file(path: &Path, record: &OpRecord) -> std::io::Result<()> {
    let _guard = session_file_lock().lock().unwrap();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(record)?;
    writeln!(file, "{}", line)?;
    Ok(())
}

/// Reads all operation records from a sidecar `.oplog.jsonl` file under session file lock.
pub fn load_op_records_from_file(path: &Path) -> std::io::Result<Vec<OpRecord>> {
    let _guard = session_file_lock().lock().unwrap();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let line_trimmed = line.trim();
        if line_trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<OpRecord>(line_trimmed) {
            records.push(rec);
        }
    }

    Ok(records)
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryResult {
    pub recovered_open_operations: usize,
    pub open_operation_ids: Vec<String>,
    pub unreplayable_tools: usize,
    pub safe_tools_to_replay: Vec<OpRecord>,
}

#[derive(Debug, Clone)]
pub struct InterruptedSubagentLane {
    pub lane: String,
    pub run_id: String,
    pub task: String,
    pub messages: Vec<AgentMessage>,
    pub safe_tools: Vec<OpRecord>,
    pub unsafe_tools: Vec<OpRecord>,
}

pub fn interrupted_subagent_lanes(records: &[OpRecord]) -> Vec<InterruptedSubagentLane> {
    let finished_runs: HashSet<&str> = records
        .iter()
        .filter_map(|record| match record {
            OpRecord::OperationFinished { lane, run_id, .. } if lane.starts_with("subagent-") => {
                Some(run_id.as_str())
            }
            _ => None,
        })
        .collect();
    let mut lanes = Vec::new();
    let mut seen_runs = HashSet::new();

    for record in records {
        let OpRecord::OperationStarted { id, lane, .. } = record else {
            continue;
        };
        if !lane.starts_with("subagent-")
            || finished_runs.contains(id.as_str())
            || !seen_runs.insert(id.as_str())
        {
            continue;
        }

        let task = records
            .iter()
            .find_map(|record| match record {
                OpRecord::TaskAttempt {
                    lane: attempt_lane,
                    run_id,
                    task,
                    ..
                } if attempt_lane == lane && run_id == id => Some(task.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let mut messages: Vec<(u64, AgentMessage)> = records
            .iter()
            .filter_map(|record| match record {
                OpRecord::WriteDeferred {
                    lane: deferred_lane,
                    run_id,
                    seq,
                    target,
                    ..
                } if deferred_lane == lane && run_id == id => Some((*seq, target.clone())),
                _ => None,
            })
            .collect();
        let completed_tool_calls: HashSet<String> = messages
            .iter()
            .filter_map(|(_, message)| match message {
                AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();
        let mut seen_tool_calls = HashSet::new();
        let mut safe_tools = Vec::new();
        let mut unsafe_tools = Vec::new();

        for record in records {
            let OpRecord::ToolStarted {
                lane: tool_lane,
                run_id,
                seq,
                tool_call_id,
                tool_name,
                replay,
                ..
            } = record
            else {
                continue;
            };
            if tool_lane != lane
                || run_id != id
                || completed_tool_calls.contains(tool_call_id)
                || !seen_tool_calls.insert(tool_call_id.as_str())
            {
                continue;
            }

            match replay {
                ToolReplaySafety::Safe => safe_tools.push(record.clone()),
                ToolReplaySafety::Never => {
                    unsafe_tools.push(record.clone());
                    messages.push((
                        *seq,
                        AgentMessage::Tool {
                            tool_call_id: tool_call_id.clone(),
                            name: tool_name.clone(),
                            content: format!(
                                "[Interrupted tool execution for '{tool_name}' automatically recovered]"
                            ),
                            is_error: true,
                        },
                    ));
                }
            }
        }
        messages.sort_by_key(|(seq, _)| *seq);

        lanes.push(InterruptedSubagentLane {
            lane: lane.clone(),
            run_id: id.clone(),
            task,
            messages: messages.into_iter().map(|(_, message)| message).collect(),
            safe_tools,
            unsafe_tools,
        });
    }

    lanes
}

/// Reconciles open operations and interrupted tool turns from operation records.
/// Returns RecoveryResult detailing recovered open operations and safe tools to replay.
pub fn reconcile_op_log_recovery(session_tree: &mut SessionTree, records: &[OpRecord]) -> RecoveryResult {
    let mut open_operations: HashSet<String> = HashSet::new();
    let mut tool_intents: Vec<&OpRecord> = Vec::new();

    for record in records {
        if record.lane().starts_with("subagent-") {
            continue;
        }
        match record {
            OpRecord::OperationStarted { id, .. } => {
                open_operations.insert(id.clone());
            }
            OpRecord::OperationFinished { run_id, .. } => {
                open_operations.remove(run_id);
            }
            rec @ OpRecord::ToolStarted { .. } => {
                tool_intents.push(rec);
            }
            _ => {}
        }
    }

    if open_operations.is_empty() {
        return RecoveryResult::default();
    }

    let mut open_operation_ids: Vec<String> = open_operations.iter().cloned().collect();
    open_operation_ids.sort();

    // Check for ToolStarted records belonging to open operations that have no matching result entry
    let existing_tool_ids: HashSet<String> = session_tree
        .nodes
        .values()
        .filter_map(|node| match &node.message {
            AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();

    let mut safe_tools_to_replay = Vec::new();
    let mut unreplayable_tools = 0;

    for intent in tool_intents {
        if let OpRecord::ToolStarted {
            run_id,
            assistant_entry_id,
            tool_call_id,
            tool_name,
            replay,
            ..
        } = intent
        {
            if open_operations.contains(run_id) && !existing_tool_ids.contains(tool_call_id) {
                if replay == &ToolReplaySafety::Never {
                    unreplayable_tools += 1;
                    let synthetic_msg = AgentMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        name: tool_name.clone(),
                        content: format!("[Interrupted tool execution for '{tool_name}' automatically recovered]"),
                        is_error: true,
                    };
                    let anchor = if session_tree.nodes.contains_key(assistant_entry_id) {
                        Some(assistant_entry_id.clone())
                    } else {
                        session_tree.active_node_id().map(String::from)
                    };
                    session_tree.add_message_at_leaf(anchor.as_deref(), synthetic_msg);
                } else if replay == &ToolReplaySafety::Safe {
                    let synthetic_msg = AgentMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        name: tool_name.clone(),
                        content: format!("[Recovered tool result for read-only operation '{tool_name}']"),
                        is_error: false,
                    };
                    let anchor = if session_tree.nodes.contains_key(assistant_entry_id) {
                        Some(assistant_entry_id.clone())
                    } else {
                        session_tree.active_node_id().map(String::from)
                    };
                    session_tree.add_message_at_leaf(anchor.as_deref(), synthetic_msg);
                    safe_tools_to_replay.push((*intent).clone());
                }
            }
        }
    }

    RecoveryResult {
        recovered_open_operations: open_operations.len(),
        open_operation_ids,
        unreplayable_tools,
        safe_tools_to_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_subagent_lanes_group_deferred_messages_by_open_run() {
        let records = vec![
            OpRecord::OperationStarted {
                id: "open-run".into(),
                seq: 1,
                lane: "subagent-1:0".into(),
                timestamp: 1,
                source_leaf_id: None,
                kind: "subagent".into(),
                system_prompt_override: None,
            },
            OpRecord::TaskAttempt {
                id: "attempt-open".into(),
                seq: 2,
                lane: "subagent-1:0".into(),
                timestamp: 2,
                run_id: "open-run".into(),
                task: "inspect".into(),
                attempt: 1,
            },
            OpRecord::WriteDeferred {
                id: "write-later".into(),
                seq: 4,
                lane: "subagent-1:0".into(),
                timestamp: 4,
                run_id: "open-run".into(),
                target: AgentMessage::User {
                    content: "second".into(),
                },
            },
            OpRecord::WriteDeferred {
                id: "write-first".into(),
                seq: 3,
                lane: "subagent-1:0".into(),
                timestamp: 3,
                run_id: "open-run".into(),
                target: AgentMessage::User {
                    content: "first".into(),
                },
            },
            OpRecord::ToolStarted {
                id: "safe-tool".into(),
                seq: 5,
                lane: "subagent-1:0".into(),
                timestamp: 5,
                run_id: "open-run".into(),
                assistant_entry_id: String::new(),
                tool_index: 0,
                tool_call_id: "call-safe".into(),
                tool_name: "read_file".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "result-safe".into(),
                replay: ToolReplaySafety::Safe,
            },
            OpRecord::ToolStarted {
                id: "duplicate-tool".into(),
                seq: 6,
                lane: "subagent-1:0".into(),
                timestamp: 6,
                run_id: "open-run".into(),
                assistant_entry_id: String::new(),
                tool_index: 1,
                tool_call_id: "call-safe".into(),
                tool_name: "read_file".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "result-safe-duplicate".into(),
                replay: ToolReplaySafety::Never,
            },
            OpRecord::WriteDeferred {
                id: "existing-result".into(),
                seq: 7,
                lane: "subagent-1:0".into(),
                timestamp: 7,
                run_id: "open-run".into(),
                target: AgentMessage::Tool {
                    tool_call_id: "call-complete".into(),
                    name: "read_file".into(),
                    content: "done".into(),
                    is_error: false,
                },
            },
            OpRecord::ToolStarted {
                id: "completed-tool".into(),
                seq: 8,
                lane: "subagent-1:0".into(),
                timestamp: 8,
                run_id: "open-run".into(),
                assistant_entry_id: String::new(),
                tool_index: 2,
                tool_call_id: "call-complete".into(),
                tool_name: "read_file".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "result-complete".into(),
                replay: ToolReplaySafety::Safe,
            },
            OpRecord::OperationStarted {
                id: "finished-run".into(),
                seq: 9,
                lane: "subagent-1:1".into(),
                timestamp: 9,
                source_leaf_id: None,
                kind: "subagent".into(),
                system_prompt_override: None,
            },
            OpRecord::TaskAttempt {
                id: "attempt-finished".into(),
                seq: 10,
                lane: "subagent-1:1".into(),
                timestamp: 10,
                run_id: "finished-run".into(),
                task: "finished task".into(),
                attempt: 1,
            },
            OpRecord::OperationFinished {
                id: "finish".into(),
                seq: 11,
                lane: "subagent-1:1".into(),
                timestamp: 11,
                run_id: "finished-run".into(),
                outcome: OpOutcome::Completed,
                error: None,
            },
        ];

        let lanes = interrupted_subagent_lanes(&records);

        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].lane, "subagent-1:0");
        assert_eq!(lanes[0].run_id, "open-run");
        assert_eq!(lanes[0].task, "inspect");
        assert!(matches!(
            &lanes[0].messages[..],
            [
                AgentMessage::User { content: first },
                AgentMessage::User { content: second },
                AgentMessage::Tool { tool_call_id, .. },
            ] if first == "first" && second == "second" && tool_call_id == "call-complete"
        ));
        assert_eq!(lanes[0].safe_tools.len(), 1);
        assert_eq!(lanes[0].safe_tools[0].id(), "safe-tool");
        assert!(lanes[0].unsafe_tools.is_empty());
    }

    #[test]
    fn unsafe_interrupted_tool_is_synthesized_once() {
        let records = vec![
            OpRecord::OperationStarted {
                id: "run-1".into(),
                seq: 1,
                lane: "subagent-1:0".into(),
                timestamp: 1,
                source_leaf_id: None,
                kind: "subagent".into(),
                system_prompt_override: None,
            },
            OpRecord::TaskAttempt {
                id: "attempt-1".into(),
                seq: 2,
                lane: "subagent-1:0".into(),
                timestamp: 2,
                run_id: "run-1".into(),
                task: "change files".into(),
                attempt: 1,
            },
            OpRecord::ToolStarted {
                id: "unsafe-tool".into(),
                seq: 3,
                lane: "subagent-1:0".into(),
                timestamp: 3,
                run_id: "run-1".into(),
                assistant_entry_id: String::new(),
                tool_index: 0,
                tool_call_id: "call-write".into(),
                tool_name: "write_file".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "result-write".into(),
                replay: ToolReplaySafety::Never,
            },
            OpRecord::ToolStarted {
                id: "duplicate-unsafe-tool".into(),
                seq: 4,
                lane: "subagent-1:0".into(),
                timestamp: 4,
                run_id: "run-1".into(),
                assistant_entry_id: String::new(),
                tool_index: 1,
                tool_call_id: "call-write".into(),
                tool_name: "write_file".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "result-write-duplicate".into(),
                replay: ToolReplaySafety::Never,
            },
        ];

        let lanes = interrupted_subagent_lanes(&records);

        assert_eq!(lanes.len(), 1);
        assert!(lanes[0].safe_tools.is_empty());
        assert_eq!(lanes[0].unsafe_tools.len(), 1);
        assert!(matches!(
            &lanes[0].messages[..],
            [AgentMessage::Tool { tool_call_id, name, is_error, .. }]
                if tool_call_id == "call-write" && name == "write_file" && *is_error
        ));
    }

    #[test]
    fn reconcile_op_log_recovery_repairs_interrupted_never_replay_tools() {
        let mut tree = SessionTree::new("test_session");
        let assistant_id = tree.add_message(AgentMessage::Assistant {
            content: Some("Calling tool".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });

        let records = vec![
            OpRecord::OperationStarted {
                id: "run-1".into(),
                seq: 1,
                lane: "main".into(),
                timestamp: 100,
                source_leaf_id: None,
                kind: "run".into(),
                system_prompt_override: None,
            },
            OpRecord::ToolStarted {
                id: "tool-rec-1".into(),
                seq: 2,
                lane: "main".into(),
                timestamp: 105,
                run_id: "run-1".into(),
                assistant_entry_id: assistant_id.clone(),
                tool_index: 0,
                tool_call_id: "call_123".into(),
                tool_name: "run_command".into(),
                effective_args: serde_json::json!({"command": "cargo test"}),
                result_entry_id: "res-1".into(),
                replay: ToolReplaySafety::Never,
            },
        ];

        let recovered = reconcile_op_log_recovery(&mut tree, &records);
        assert_eq!(recovered.recovered_open_operations, 1);
        assert_eq!(recovered.open_operation_ids, vec!["run-1"]);
        assert_eq!(recovered.unreplayable_tools, 1);

        let branch = tree.get_active_branch_messages();
        assert_eq!(branch.len(), 2);
        assert!(matches!(
            &branch[1],
            AgentMessage::Tool { tool_call_id, is_error, .. } if tool_call_id == "call_123" && *is_error
        ));
    }

    #[test]
    fn oplog_file_round_trip_under_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.oplog.jsonl");

        let rec = OpRecord::OperationStarted {
            id: "run-99".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 200,
            source_leaf_id: None,
            kind: "run".into(),
            system_prompt_override: None,
        };

        append_op_record_to_file(&path, &rec).unwrap();
        let loaded = load_op_records_from_file(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id(), "run-99");
    }

    #[test]
    fn test_priority_steer_queue_ordering() {
        let mut queue = LaneQueue::default();
        queue.enqueue_steer_with_priority(AgentMessage::User { content: "normal".into() }, SteerPriority::Normal);
        queue.enqueue_steer_with_priority(AgentMessage::User { content: "low".into() }, SteerPriority::Low);
        queue.enqueue_steer_with_priority(AgentMessage::User { content: "high interrupt".into() }, SteerPriority::High);

        let popped1 = queue.pop_steer().unwrap();
        let popped2 = queue.pop_steer().unwrap();
        let popped3 = queue.pop_steer().unwrap();

        assert_eq!(popped1.role_str(), "user");
        assert_eq!(popped2.role_str(), "user");
        assert_eq!(popped3.role_str(), "user");

        if let AgentMessage::User { content } = popped1 {
            assert_eq!(content, "high interrupt");
        }
        if let AgentMessage::User { content } = popped2 {
            assert_eq!(content, "normal");
        }
        if let AgentMessage::User { content } = popped3 {
            assert_eq!(content, "low");
        }
    }
}
