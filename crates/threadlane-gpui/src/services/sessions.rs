//! Session operations and projections for the GPUI frontend.

use std::path::Path;

use crate::state::{load_session_messages, ChatMessageInfo};

pub fn load_messages(session_file: &Path) -> Vec<ChatMessageInfo> {
    load_session_messages(session_file)
}
