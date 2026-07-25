use crate::types::{AgentMessage, QueueMode};

#[derive(Debug, Clone)]
pub struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    pub(crate) fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    pub(crate) fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    pub(crate) fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    fn len(&self) -> usize {
        self.messages.len()
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub(crate) fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}
