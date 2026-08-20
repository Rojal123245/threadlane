use crate::types::{AgentMessage, SessionPlan};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub(crate) timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) seq: Option<u64>,
    pub message: AgentMessage,
}

#[derive(Debug, Clone, Default)]
pub struct SessionTree {
    pub session_id: String,
    pub name: Option<String>,
    pub title_attempted: bool,
    pub model: Option<String>,
    pub parent_session_id: Option<String>,
    pub plan: SessionPlan,
    pub global_facts: HashMap<String, String>,
    pub nodes: HashMap<String, SessionNode>,
    /// Node IDs in persisted/insertion order. This is intentionally separate
    /// from `nodes`: the map is only an index and does not define ordering.
    pub node_order: Vec<String>,
    active_node_id: Option<String>,
    pub file_path: Option<PathBuf>,
    pub v2_lines: Vec<String>,
    pub v2_entry_ids: HashSet<String>,
}

impl SessionTree {
    fn next_persisted_seq(&self) -> u64 {
        let max_seq = self
            .node_order
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                self.nodes
                    .get(id)
                    .map(|node| node.seq.unwrap_or(index as u64 + 1))
            })
            .max()
            .unwrap_or(0);
        for line in &self.v2_lines {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                let seq = value
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .or_else(|| {
                        value
                            .as_object()
                            .and_then(|object| object.values().next())
                            .and_then(|record| record.get("seq"))
                            .and_then(serde_json::Value::as_u64)
                    })
                    .unwrap_or(0);
                let _ = max_seq.max(seq);
            }
        }
        max_seq + 1
    }

    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            name: None,
            title_attempted: false,
            model: None,
            parent_session_id: None,
            plan: SessionPlan::default(),
            global_facts: HashMap::new(),
            nodes: HashMap::new(),
            node_order: Vec::new(),
            active_node_id: None,
            file_path: None,
            v2_lines: Vec::new(),
            v2_entry_ids: HashSet::new(),
        }
    }

    pub fn is_v2(&self) -> bool {
        true
    }

    pub fn has_name(&self) -> bool {
        self.name.as_ref().is_some_and(|name| !name.is_empty())
    }

    pub fn active_node_id(&self) -> Option<&str> {
        self.active_node_id.as_deref()
    }

    pub(crate) fn project_harness_entry(&mut self, entry: &crate::harness::Entry) {
        if !self.v2_entry_ids.insert(entry.id.clone()) {
            return;
        }
        self.node_order.push(entry.id.clone());
        self.nodes.insert(
            entry.id.clone(),
            SessionNode {
                id: entry.id.clone(),
                parent_id: entry.parent_id.clone(),
                timestamp: entry.timestamp,
                seq: Some(entry.seq),
                message: entry.message.clone(),
            },
        );
        if entry.lane == "main" {
            self.active_node_id = Some(entry.id.clone());
        }
    }

    pub(crate) fn project_harness_record(&mut self, record: &crate::harness::Record) {
        if let crate::harness::Record::FactSet {
            run_id: None,
            key,
            value,
            ..
        } = record
        {
            self.global_facts.insert(key.clone(), value.clone());
            match key.as_str() {
                "model" => self.model = Some(value.clone()),
                "name" => self.name = Some(value.clone()),
                "parent_session_id" => self.parent_session_id = Some(value.clone()),
                "session_plan" => {
                    if let Ok(plan) = serde_json::from_str::<crate::SessionPlan>(value) {
                        self.plan = plan;
                    }
                }
                _ => {}
            }
        }
    }

    pub fn get_fact(&self, key: &str) -> Option<&str> {
        self.global_facts.get(key).map(|s| s.as_str())
    }

    pub fn set_fact(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> std::io::Result<()> {
        self.global_facts.insert(key.into(), value.into());
        Ok(())
    }

    pub fn set_fact_in_memory(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.global_facts.insert(key.into(), value.into());
    }

    pub fn plan(&self) -> &SessionPlan {
        &self.plan
    }

    pub fn set_name(&mut self, name: String) -> std::io::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session name cannot be empty",
            ));
        }
        self.name = Some(name.to_string());
        Ok(())
    }

    pub fn set_model(&mut self, model: String) -> std::io::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session model cannot be empty",
            ));
        }
        self.model = Some(model.to_string());
        Ok(())
    }

    pub fn set_model_in_memory(&mut self, model: String) -> std::io::Result<()> {
        self.set_model(model)
    }

    pub fn set_name_in_memory(&mut self, name: String) -> std::io::Result<()> {
        self.set_name(name)
    }

    pub fn mark_title_attempted(&mut self) -> std::io::Result<bool> {
        if self.title_attempted {
            return Ok(false);
        }
        self.title_attempted = true;
        Ok(true)
    }

    pub fn add_message(&mut self, message: AgentMessage) -> String {
        let leaf_id = self.active_node_id.clone();
        self.add_message_at_leaf(leaf_id.as_deref(), message)
    }

    pub fn add_message_in_memory(&mut self, message: AgentMessage) -> String {
        self.add_message(message)
    }

    pub fn add_message_at_leaf(&mut self, leaf_id: Option<&str>, message: AgentMessage) -> String {
        let mut next_id = self.nodes.len() + 1;
        let node_id = loop {
            let candidate = format!("node_{next_id}");
            if !self.nodes.contains_key(&candidate) {
                break candidate;
            }
            next_id += 1;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let parent_id = leaf_id.map(|s| s.to_string());
        let node = SessionNode {
            id: node_id.clone(),
            parent_id,
            timestamp: now,
            seq: Some(self.next_persisted_seq()),
            message,
        };

        let old_active = self.active_node_id.clone();
        self.nodes.insert(node_id.clone(), node);
        self.node_order.push(node_id.clone());

        if leaf_id == old_active.as_deref() || old_active.is_none() {
            self.active_node_id = Some(node_id.clone());
        }

        node_id
    }

    pub fn append_passive_branch(
        &mut self,
        parent_leaf_id: Option<&str>,
        messages: Vec<AgentMessage>,
    ) -> Result<Vec<String>, String> {
        self.append_passive_branch_in_memory(parent_leaf_id, messages)
    }

    pub fn append_passive_branch_in_memory(
        &mut self,
        parent_leaf_id: Option<&str>,
        messages: Vec<AgentMessage>,
    ) -> Result<Vec<String>, String> {
        if let Some(parent_leaf_id) = parent_leaf_id {
            if !self.nodes.contains_key(parent_leaf_id) {
                return Err(format!("Parent session node '{parent_leaf_id}' not found"));
            }
        }
        let active_node_id = self.active_node_id.clone();
        let mut parent_id = parent_leaf_id.map(str::to_owned);
        let mut created = Vec::with_capacity(messages.len());
        for message in messages {
            let mut next_id = self.nodes.len() + 1;
            let node_id = loop {
                let candidate = format!("node_{next_id}");
                if !self.nodes.contains_key(&candidate) {
                    break candidate;
                }
                next_id += 1;
            };
            let node = SessionNode {
                id: node_id.clone(),
                parent_id: parent_id.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                seq: Some(self.next_persisted_seq()),
                message,
            };
            self.nodes.insert(node_id.clone(), node);
            self.node_order.push(node_id.clone());
            parent_id = Some(node_id.clone());
            created.push(node_id);
        }
        self.active_node_id = active_node_id;
        Ok(created)
    }

    pub fn replace_active_branch(&mut self, messages: Vec<AgentMessage>) {
        self.active_node_id = None;
        for message in messages {
            if !matches!(message, AgentMessage::System { .. }) {
                self.add_message(message);
            }
        }
    }

    pub fn replace_active_branch_in_memory(&mut self, messages: Vec<AgentMessage>) {
        self.replace_active_branch(messages);
    }

    pub fn get_active_branch_messages(&self) -> Vec<AgentMessage> {
        self.get_branch_messages(self.active_node_id.as_deref())
    }

    pub fn get_branch_messages(&self, leaf_id: Option<&str>) -> Vec<AgentMessage> {
        let mut path_nodes = Vec::new();
        let mut curr = leaf_id.map(|s| s.to_string());

        while let Some(id) = curr {
            if let Some(node) = self.nodes.get(&id) {
                path_nodes.push(node.message.clone());
                curr = node.parent_id.clone();
            } else {
                break;
            }
        }

        path_nodes.reverse();
        path_nodes
    }

    pub fn get_persisted_messages(&self) -> Vec<AgentMessage> {
        self.node_order
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .map(|node| node.message.clone())
            .collect()
    }

    pub fn replace_tool_result(
        &mut self,
        tool_call_id: &str,
        content: String,
        is_error: bool,
    ) -> bool {
        let Some(node_id) = self
            .node_order
            .iter()
            .find(|node_id| {
                matches!(
                    self.nodes.get(*node_id).map(|node| &node.message),
                    Some(AgentMessage::Tool { tool_call_id: id, .. }) if id == tool_call_id
                )
            })
            .cloned()
        else {
            return false;
        };
        let Some(node) = self.nodes.get_mut(&node_id) else {
            return false;
        };
        if let AgentMessage::Tool {
            content: current_content,
            is_error: current_is_error,
            ..
        } = &mut node.message
        {
            *current_content = content;
            *current_is_error = is_error;
        }
        true
    }

    pub fn switch_active_node(&mut self, node_id: &str) -> bool {
        if !self.nodes.contains_key(node_id) {
            return false;
        }
        self.active_node_id = Some(node_id.to_string());
        true
    }

    pub fn fork_branch(&mut self, node_id: &str) -> Option<SessionTree> {
        if !self.nodes.contains_key(node_id) {
            return None;
        }

        let suffix = node_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let new_id = format!("{}_fork_{}", self.session_id, suffix);
        let mut forked = SessionTree::new(new_id);
        forked.parent_session_id = Some(self.session_id.clone());
        forked.name = self.name.clone();
        forked.title_attempted = self.title_attempted;
        forked.model = self.model.clone();
        forked.plan = self.plan.clone();
        forked.global_facts = self.global_facts.clone();

        let mut curr = Some(node_id.to_string());
        let mut path_nodes = Vec::new();

        while let Some(id) = curr {
            if let Some(node) = self.nodes.get(&id) {
                path_nodes.push(node.clone());
                curr = node.parent_id.clone();
            } else {
                break;
            }
        }
        path_nodes.reverse();

        for node in path_nodes {
            forked.add_message(node.message);
        }

        Some(forked)
    }

    pub fn load_from_file(path: &Path) -> std::io::Result<Self> {
        let session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());

        let mut tree = SessionTree::new(session_id);
        tree.file_path = Some(path.to_path_buf());

        if !path.exists() {
            return Ok(tree);
        }
        let data = std::fs::read_to_string(path)?;

        let mut v2_main_leaf = None;
        let line_count = data.split('\n').count();
        for (line_number, l) in data.split('\n').enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            let torn_tail = line_number + 1 == line_count && !data.ends_with('\n');
            let value = match serde_json::from_str::<serde_json::Value>(l) {
                Ok(value) => value,
                Err(_error) if torn_tail => break,
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Failed to parse session line {}: {error}", line_number + 1),
                    ));
                }
            };
            if let Ok(record) = serde_json::from_value::<crate::harness::Record>(value.clone()) {
                if let crate::harness::Record::FactSet {
                    run_id: None,
                    key,
                    value,
                    ..
                } = record
                {
                    tree.global_facts.insert(key.clone(), value.clone());
                    match key.as_str() {
                        "model" => tree.model = Some(value),
                        "name" => tree.name = Some(value),
                        "parent_session_id" => tree.parent_session_id = Some(value),
                        "session_plan" => {
                            if let Ok(plan) = serde_json::from_str::<crate::SessionPlan>(&value) {
                                tree.plan = plan;
                            }
                        }
                        _ => {}
                    }
                }
                tree.v2_lines.push(l.to_owned());
                continue;
            }
            if value.get("lane").is_some() {
                if let Ok(entry) = serde_json::from_value::<crate::harness::Entry>(value.clone()) {
                    if entry.lane == "main" {
                        v2_main_leaf = Some(entry.id.clone());
                    }
                    tree.node_order.push(entry.id.clone());
                    tree.v2_entry_ids.insert(entry.id.clone());
                    tree.nodes.insert(
                        entry.id.clone(),
                        SessionNode {
                            id: entry.id,
                            parent_id: entry.parent_id,
                            timestamp: entry.timestamp,
                            seq: Some(entry.seq),
                            message: entry.message,
                        },
                    );
                    tree.v2_lines.push(l.to_owned());
                    continue;
                }
            }
            if let Ok(node) = serde_json::from_value::<SessionNode>(value) {
                v2_main_leaf = Some(node.id.clone());
                tree.node_order.push(node.id.clone());
                tree.nodes.insert(node.id.clone(), node);
                tree.v2_lines.push(l.to_owned());
                continue;
            }
        }

        if let Some(leaf_id) = v2_main_leaf {
            tree.active_node_id = Some(leaf_id);
        }

        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::SessionStore;

    #[test]
    fn direct_load_applies_v2_model_and_name_facts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut store = crate::harness::JsonlStore::open(&path).unwrap();
        store
            .append_entry(crate::harness::Entry::new(
                "entry-1",
                None,
                "main",
                1,
                100,
                AgentMessage::user("hello", vec![]),
                false,
            ))
            .unwrap();
        store
            .append_record(crate::harness::Record::FactSet {
                id: "fact-model".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: None,
                key: "model".into(),
                value: "antigravity/provider-model".into(),
            })
            .unwrap();
        store
            .append_record(crate::harness::Record::FactSet {
                id: "fact-name".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 2,
                run_id: None,
                key: "name".into(),
                value: "Durable title".into(),
            })
            .unwrap();

        let loaded = SessionTree::load_from_file(&path).unwrap();
        assert_eq!(loaded.model.as_deref(), Some("antigravity/provider-model"));
        assert_eq!(loaded.name.as_deref(), Some("Durable title"));
        assert_eq!(loaded.get_fact("model"), Some("antigravity/provider-model"));
    }

    #[test]
    fn set_name_rejects_empty_name() {
        let mut tree = SessionTree::new("session");
        assert!(tree.set_name(String::new()).is_err());
        assert!(tree.name.is_none());
    }

    #[test]
    fn passive_dag_branch_retrieval_by_leaf() {
        let mut tree = SessionTree::new("passive_session");
        let n1 = tree.add_message(AgentMessage::User {
            content: "root".into(),
        });
        let n2 = tree.add_message_at_leaf(
            Some(&n1),
            AgentMessage::User {
                content: "lane_a_1".into(),
            },
        );
        let n3 = tree.add_message_at_leaf(
            Some(&n1),
            AgentMessage::User {
                content: "lane_b_1".into(),
            },
        );

        let branch_a = tree.get_branch_messages(Some(&n2));
        assert_eq!(branch_a.len(), 2);
        assert!(matches!(&branch_a[1], AgentMessage::User { content } if content == "lane_a_1"));

        let branch_b = tree.get_branch_messages(Some(&n3));
        assert_eq!(branch_b.len(), 2);
        assert!(matches!(&branch_b[1], AgentMessage::User { content } if content == "lane_b_1"));
    }

    #[test]
    fn passive_branch_append_preserves_active_leaf_and_order() {
        let mut tree = SessionTree::new("session");
        let parent = tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });
        let expected = vec![
            AgentMessage::User {
                content: "child task".into(),
            },
            AgentMessage::Assistant {
                content: Some("child result".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];

        let branch = tree
            .append_passive_branch(Some(&parent), expected.clone())
            .unwrap();

        assert_eq!(tree.active_node_id(), Some(parent.as_str()));
        assert_eq!(
            serde_json::to_value(tree.get_branch_messages(branch.last().map(String::as_str)))
                .unwrap(),
            serde_json::to_value(
                [
                    vec![AgentMessage::User {
                        content: "parent".into()
                    }],
                    expected
                ]
                .concat()
            )
            .unwrap()
        );
    }

    #[test]
    fn global_facts_in_memory_lookup() {
        let mut tree = SessionTree::new("facts_session");
        tree.set_fact("git_branch", "feat/multi-lane").unwrap();
        tree.set_fact("env", "staging").unwrap();

        assert_eq!(tree.get_fact("git_branch"), Some("feat/multi-lane"));
        assert_eq!(tree.get_fact("env"), Some("staging"));
    }

    #[test]
    fn v2_sessions_are_read_only_to_legacy_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2_session.jsonl");

        let entry = crate::harness::Entry::new(
            "entry-1",
            None,
            "main",
            1,
            100,
            AgentMessage::user("hello v2", Vec::new()),
            false,
        );
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&entry).unwrap())).unwrap();

        let mut tree = SessionTree::load_from_file(&path).unwrap();
        assert!(tree.is_v2());

        // Attempting to add a message should update in-memory nodes but not write legacy node line to disk
        let node_id = tree.add_message(AgentMessage::user("followup", Vec::new()));
        assert!(!node_id.is_empty());

        let raw_file = std::fs::read_to_string(&path).unwrap();
        assert!(!raw_file.contains("node_"));
        assert!(!raw_file.contains("followup"));

        tree.set_fact("v2_fact", "value").unwrap();
        assert_eq!(tree.get_fact("v2_fact"), Some("value"));
        let raw_file_after_fact = std::fs::read_to_string(&path).unwrap();
        assert!(!raw_file_after_fact.contains("global_fact"));
    }
}
