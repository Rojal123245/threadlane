//! Per-lane message queue and subagent recovery.
//!
//! The record types (`Record`, `OperationOutcome`, `QueueKind`, `ToolReplaySafety`)
//! are now canonical in [`crate::harness`]. This module provides the in-memory
//! message queue and the subagent-interruption recovery logic built on top of
//! those harness types.

#[allow(unused_imports)]
use crate::harness::{OperationOutcome, ProvisionedEntry, QueueKind, Record, ToolReplaySafety};
use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

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

pub fn interrupted_subagent_lanes(records: &[Record]) -> Vec<InterruptedSubagentLane> {
    struct Occurrence {
        lane: String,
        run_id: String,
        started_seq: u64,
        source_leaf_id: Option<String>,
        task: String,
        task_attempted: bool,
        messages: Vec<(u64, AgentMessage)>,
        tools: Vec<Record>,
        completed_tools: HashSet<String>,
        active: bool,
    }

    let mut ordered: Vec<_> = records.iter().enumerate().collect();
    ordered.sort_by_key(|(index, record)| (record.seq(), *index));
    let mut occurrences = Vec::new();
    let mut active: HashMap<(String, String), Vec<usize>> = HashMap::new();

    for (_, record) in ordered {
        match record {
            Record::OperationStarted {
                id,
                lane,
                seq,
                source_leaf_id,
                intent: crate::harness::OperationIntent::Run,
                ..
            } => {
                let index = occurrences.len();
                occurrences.push(Occurrence {
                    lane: lane.clone(),
                    run_id: id.clone(),
                    started_seq: *seq,
                    source_leaf_id: source_leaf_id.clone(),
                    task: String::new(),
                    task_attempted: false,
                    messages: Vec::new(),
                    tools: Vec::new(),
                    completed_tools: HashSet::new(),
                    active: true,
                });
                active
                    .entry((lane.clone(), id.clone()))
                    .or_default()
                    .push(index);
            }
            Record::StepAttempt { lane, run_id, .. } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index].task_attempted = true;
                }
            }
            Record::WriteDeferred {
                lane,
                run_id,
                seq,
                target,
                ..
            } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index]
                        .messages
                        .push((*seq, target.message.clone()));
                }
            }
            Record::ToolStarted { lane, run_id, .. } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index].tools.push(record.clone());
                }
            }
            Record::ToolFinished {
                lane,
                run_id,
                tool_call_id,
                ..
            } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index]
                        .completed_tools
                        .insert(tool_call_id.clone());
                }
            }
            Record::OperationFinished { lane, run_id, .. } => {
                let key = (lane.clone(), run_id.clone());
                let remove_key = if let Some(occurrences_for_run) = active.get_mut(&key) {
                    if let Some(index) = occurrences_for_run.pop() {
                        occurrences[index].active = false;
                    }
                    occurrences_for_run.is_empty()
                } else {
                    false
                };
                if remove_key {
                    active.remove(&key);
                }
            }
            _ => {}
        }
    }

    let mut lanes = Vec::new();
    for occurrence in occurrences
        .into_iter()
        .filter(|occurrence| occurrence.active)
    {
        let mut messages = occurrence.messages;
        let mut completed_tool_calls: HashSet<String> = messages
            .iter()
            .filter_map(|(_, message)| match message {
                AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();
        completed_tool_calls.extend(occurrence.completed_tools);
        let mut tools: HashMap<String, (Option<Record>, Option<Record>)> = HashMap::new();

        for record in occurrence.tools {
            let Record::ToolStarted {
                tool_call_id,
                replay,
                ..
            } = &record
            else {
                continue;
            };
            if completed_tool_calls.contains(tool_call_id) {
                continue;
            }
            let entry = tools.entry(tool_call_id.clone()).or_default();
            match replay {
                ToolReplaySafety::Safe if entry.0.is_none() => entry.0 = Some(record),
                ToolReplaySafety::Never if entry.1.is_none() => entry.1 = Some(record),
                _ => {}
            }
        }

        let mut safe_tools = Vec::new();
        let mut unsafe_tools = Vec::new();
        for (_, (safe, never)) in tools {
            if let Some(record) = never {
                if let Record::ToolStarted {
                    seq,
                    tool_call_id,
                    tool_name,
                    ..
                } = &record
                {
                    messages.push((
                        *seq,
                        AgentMessage::Tool {
                            tool_call_id: tool_call_id.clone(),
                            name: tool_name.clone(),
                            content: format!(
                                "[Interrupted tool execution for '{tool_name}' automatically recovered]"
                            ),
                            is_error: true,
                            terminate: false,
                        },
                    ));
                }
                unsafe_tools.push(record);
            } else if let Some(record) = safe {
                safe_tools.push(record);
            }
        }
        messages.sort_by_key(|(seq, _)| *seq);
        safe_tools.sort_by_key(Record::seq);
        unsafe_tools.sort_by_key(Record::seq);

        // Derive the task from the first User message in deferred writes
        // since Record::StepAttempt no longer carries a task field.
        let task = if occurrence.task.is_empty() {
            messages
                .iter()
                .find_map(|(_, msg)| match msg {
                    AgentMessage::User { content } => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        } else {
            occurrence.task
        };

        lanes.push((
            occurrence.started_seq,
            InterruptedSubagentLane {
                lane: occurrence.lane,
                run_id: occurrence.run_id,
                source_leaf_id: occurrence.source_leaf_id,
                started_seq: occurrence.started_seq,
                task,
                task_attempted: occurrence.task_attempted,
                messages: messages.into_iter().map(|(_, message)| message).collect(),
                safe_tools,
                unsafe_tools,
            },
        ));
    }

    lanes.sort_by(|(left_seq, left), (right_seq, right)| {
        left_seq
            .cmp(right_seq)
            .then_with(|| left.lane.cmp(&right.lane))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    lanes.into_iter().map(|(_, lane)| lane).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subagent_start(lane: &str, run_id: &str, seq: u64) -> Record {
        Record::OperationStarted {
            id: run_id.into(),
            seq,
            lane: lane.into(),
            timestamp: seq,
            source_leaf_id: None,
            intent: crate::harness::OperationIntent::Run,
        }
    }

    fn subagent_attempt(lane: &str, run_id: &str, task: &str, seq: u64) -> Record {
        // The task is stored in a WriteDeferred record as a User message
        // since Record::StepAttempt doesn't carry a task field.
        let _task = task;
        Record::StepAttempt {
            id: format!("attempt-{lane}-{seq}"),
            seq,
            lane: lane.into(),
            timestamp: seq,
            run_id: run_id.into(),
            attempt: 1,
            result_entry_id: String::new(),
            compaction_reason: None,
        }
    }

    /// Helper that bundles a StepAttempt with a preceding WriteDeferred
    /// carrying the task as a User message.
    fn subagent_attempt_with_task(
        out: &mut Vec<Record>,
        lane: &str,
        run_id: &str,
        task: &str,
        seq: u64,
    ) {
        out.push(Record::WriteDeferred {
            id: format!("task-msg-{lane}-{seq}"),
            seq,
            lane: lane.into(),
            timestamp: seq,
            run_id: run_id.into(),
            target: ProvisionedEntry {
                id: format!("provisioned-{lane}-{seq}"),
                parent_id: None,
                message: AgentMessage::User {
                    content: task.into(),
                },
            },
        });
        out.push(Record::StepAttempt {
            id: format!("attempt-{lane}-{seq}"),
            seq: seq + 1,
            lane: lane.into(),
            timestamp: seq + 1,
            run_id: run_id.into(),
            attempt: 1,
            result_entry_id: String::new(),
            compaction_reason: None,
        });
    }

    fn subagent_tool(
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        replay: ToolReplaySafety,
        seq: u64,
    ) -> Record {
        Record::ToolStarted {
            id: format!("tool-{tool_call_id}-{seq}"),
            seq,
            lane: lane.into(),
            timestamp: seq,
            run_id: run_id.into(),
            assistant_entry_id: String::new(),
            tool_index: 0,
            tool_call_id: tool_call_id.into(),
            tool_name: "tool".into(),
            effective_args: serde_json::json!({}),
            result_entry_id: format!("result-{tool_call_id}-{seq}"),
            replay,
        }
    }

    fn subagent_finish(lane: &str, run_id: &str, seq: u64) -> Record {
        Record::OperationFinished {
            id: format!("finish-{lane}-{seq}"),
            seq,
            lane: lane.into(),
            timestamp: seq,
            run_id: run_id.into(),
            outcome: OperationOutcome::Completed,
            error: None,
        }
    }

    #[test]
    fn interrupted_subagent_lanes_group_deferred_messages_by_open_run() {
        let records = vec![
            Record::OperationStarted {
                id: "open-run".into(),
                seq: 0,
                lane: "subagent-1:0".into(),
                timestamp: 0,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            },
            Record::StepAttempt {
                id: "attempt-open".into(),
                seq: 2,
                lane: "subagent-1:0".into(),
                timestamp: 2,
                run_id: "open-run".into(),
                attempt: 1,
                result_entry_id: String::new(),
                compaction_reason: None,
            },
            // Task message — the recovery logic derives the task from the
            // first User message in WriteDeferred records (sorted by seq).
            Record::WriteDeferred {
                id: "task-msg".into(),
                seq: 1,
                lane: "subagent-1:0".into(),
                timestamp: 1,
                run_id: "open-run".into(),
                target: ProvisionedEntry {
                    id: "task-msg".into(),
                    parent_id: None,
                    message: AgentMessage::User {
                        content: "inspect".into(),
                    },
                },
            },
            Record::WriteDeferred {
                id: "write-later".into(),
                seq: 4,
                lane: "subagent-1:0".into(),
                timestamp: 4,
                run_id: "open-run".into(),
                target: ProvisionedEntry {
                    id: "write-later".into(),
                    parent_id: None,
                    message: AgentMessage::User {
                        content: "second".into(),
                    },
                },
            },
            Record::WriteDeferred {
                id: "write-first".into(),
                seq: 3,
                lane: "subagent-1:0".into(),
                timestamp: 3,
                run_id: "open-run".into(),
                target: ProvisionedEntry {
                    id: "write-first".into(),
                    parent_id: None,
                    message: AgentMessage::User {
                        content: "first".into(),
                    },
                },
            },
            Record::ToolStarted {
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
            Record::ToolStarted {
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
            Record::WriteDeferred {
                id: "existing-result".into(),
                seq: 7,
                lane: "subagent-1:0".into(),
                timestamp: 7,
                run_id: "open-run".into(),
                target: ProvisionedEntry {
                    id: "existing-result".into(),
                    parent_id: None,
                    message: AgentMessage::Tool {
                        tool_call_id: "call-complete".into(),
                        name: "read_file".into(),
                        content: "done".into(),
                        is_error: false,
                        terminate: false,
                    },
                },
            },
            Record::ToolStarted {
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
            Record::OperationStarted {
                id: "finished-run".into(),
                seq: 9,
                lane: "subagent-1:1".into(),
                timestamp: 9,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            },
            Record::StepAttempt {
                id: "attempt-finished".into(),
                seq: 10,
                lane: "subagent-1:1".into(),
                timestamp: 10,
                run_id: "finished-run".into(),
                attempt: 1,
                result_entry_id: String::new(),
                compaction_reason: None,
            },
            Record::OperationFinished {
                id: "finish".into(),
                seq: 11,
                lane: "subagent-1:1".into(),
                timestamp: 11,
                run_id: "finished-run".into(),
                outcome: OperationOutcome::Completed,
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
                AgentMessage::User { content: task_msg },
                AgentMessage::User { content: first },
                AgentMessage::User { content: second },
                AgentMessage::Tool { tool_call_id: recovered, is_error: true, .. },
                AgentMessage::Tool { tool_call_id: completed, .. },
            ] if task_msg == "inspect" && first == "first" && second == "second"
                && recovered == "call-safe" && completed == "call-complete"
        ));
        assert!(lanes[0].safe_tools.is_empty());
        assert_eq!(lanes[0].unsafe_tools.len(), 1);
        assert_eq!(lanes[0].unsafe_tools[0].id(), "duplicate-tool");
    }

    #[test]
    fn unsafe_interrupted_tool_is_synthesized_once() {
        let records = vec![
            Record::OperationStarted {
                id: "run-1".into(),
                seq: 1,
                lane: "subagent-1:0".into(),
                timestamp: 1,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            },
            Record::StepAttempt {
                id: "attempt-1".into(),
                seq: 2,
                lane: "subagent-1:0".into(),
                timestamp: 2,
                run_id: "run-1".into(),
                attempt: 1,
                result_entry_id: String::new(),
                compaction_reason: None,
            },
            Record::ToolStarted {
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
            Record::ToolStarted {
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
    fn interrupted_subagent_lanes_keep_later_reused_run_open() {
        let mut records = vec![subagent_start("subagent-a", "shared", 4)];
        subagent_attempt_with_task(&mut records, "subagent-a", "shared", "fresh", 5);
        records.push(subagent_start("subagent-b", "shared", 7));
        subagent_attempt_with_task(&mut records, "subagent-b", "shared", "other lane", 8);
        records.push(subagent_start("subagent-a", "shared", 1));
        subagent_attempt_with_task(&mut records, "subagent-a", "shared", "old", 2);
        records.push(subagent_finish("subagent-a", "shared", 3));

        let lanes = interrupted_subagent_lanes(&records);

        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].lane, "subagent-a");
        assert_eq!(lanes[0].task, "fresh");
        assert_eq!(lanes[1].lane, "subagent-b");
        assert_eq!(lanes[1].task, "other lane");
    }

    #[test]
    fn never_duplicate_tool_wins_when_recorded_before_safe() {
        let records = vec![
            subagent_start("subagent-1:0", "run-1", 1),
            subagent_attempt("subagent-1:0", "run-1", "change", 2),
            subagent_tool(
                "subagent-1:0",
                "run-1",
                "call-1",
                ToolReplaySafety::Never,
                3,
            ),
            subagent_tool("subagent-1:0", "run-1", "call-1", ToolReplaySafety::Safe, 4),
        ];

        let lanes = interrupted_subagent_lanes(&records);

        assert!(lanes[0].safe_tools.is_empty());
        assert_eq!(lanes[0].unsafe_tools.len(), 1);
        assert_eq!(lanes[0].unsafe_tools[0].seq(), 3);
        assert!(matches!(
            &lanes[0].messages[..],
            [AgentMessage::Tool { tool_call_id, is_error: true, .. }] if tool_call_id == "call-1"
        ));
    }

    #[test]
    fn interrupted_subagent_lanes_sort_lanes_and_tools_by_sequence() {
        let records = vec![
            subagent_tool(
                "subagent-late",
                "run-late",
                "safe-late",
                ToolReplaySafety::Safe,
                24,
            ),
            subagent_start("subagent-late", "run-late", 20),
            subagent_tool(
                "subagent-late",
                "run-late",
                "unsafe-late",
                ToolReplaySafety::Never,
                25,
            ),
            subagent_attempt("subagent-late", "run-late", "late", 21),
            subagent_tool(
                "subagent-late",
                "run-late",
                "safe-early",
                ToolReplaySafety::Safe,
                22,
            ),
            subagent_tool(
                "subagent-late",
                "run-late",
                "unsafe-early",
                ToolReplaySafety::Never,
                23,
            ),
            subagent_attempt("subagent-early", "run-early", "early", 11),
            subagent_start("subagent-early", "run-early", 10),
        ];

        let lanes = interrupted_subagent_lanes(&records);

        assert_eq!(
            lanes
                .iter()
                .map(|lane| lane.lane.as_str())
                .collect::<Vec<_>>(),
            ["subagent-early", "subagent-late"]
        );
        assert_eq!(
            lanes[1]
                .safe_tools
                .iter()
                .map(Record::seq)
                .collect::<Vec<_>>(),
            [22, 24]
        );
        assert_eq!(
            lanes[1]
                .unsafe_tools
                .iter()
                .map(Record::seq)
                .collect::<Vec<_>>(),
            [23, 25]
        );
    }

    #[test]
    fn test_priority_steer_queue_ordering() {
        let mut queue = LaneQueue::default();
        queue.enqueue_steer_with_priority(
            AgentMessage::User {
                content: "normal".into(),
            },
            SteerPriority::Normal,
        );
        queue.enqueue_steer_with_priority(
            AgentMessage::User {
                content: "low".into(),
            },
            SteerPriority::Low,
        );
        queue.enqueue_steer_with_priority(
            AgentMessage::User {
                content: "high interrupt".into(),
            },
            SteerPriority::High,
        );

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
