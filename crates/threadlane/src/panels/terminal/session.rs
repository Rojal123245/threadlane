//! Terminal session and group management.

use makepad_terminal_core::{Pty, Terminal};

pub struct ProjectTerminalSession {
    pub pty: Pty,
    pub emulator: Terminal,
}

impl ProjectTerminalSession {
    #[allow(dead_code)]
    pub fn new(pty: Pty, emulator: Terminal) -> Self {
        Self { pty, emulator }
    }

    pub fn terminate(self) {
        let Self { pty, emulator } = self;
        drop(pty);
        drop(emulator);
    }
}

#[derive(Default)]
pub struct ProjectTerminalGroup {
    pub sessions: Vec<ProjectTerminalSession>,
    pub active: usize,
    pub error: Option<String>,
}

impl ProjectTerminalGroup {
    pub fn terminate(self) {
        for session in self.sessions {
            session.terminate();
        }
    }
}
