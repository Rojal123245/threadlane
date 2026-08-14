use super::*;
use ::log::warn;
use makepad_terminal_core::{Pty, TermKeyCode as TerminalKeyCode, Terminal};
use std::path::{Path, PathBuf};

pub(super) fn canonical_terminal_work_dir(work_dir: &Path) -> PathBuf {
    std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf())
}

pub(super) fn truncate_terminal_output(output: &mut String) {
    if output.len() <= MAX_TERMINAL_OUTPUT {
        return;
    }
    let mut cutoff = output.len() - MAX_TERMINAL_OUTPUT;
    while cutoff < output.len() && !output.is_char_boundary(cutoff) {
        cutoff += 1;
    }
    let cutoff = output[cutoff..]
        .char_indices()
        .find(|(_, ch)| *ch == '\n')
        .map(|(index, _)| cutoff + index + 1)
        .unwrap_or(cutoff);
    output.drain(..cutoff);
}

impl App {
    pub(super) fn active_terminal_project(&self) -> Option<PathBuf> {
        self.workspace_state
            .active_key()
            .map(|key| canonical_terminal_work_dir(&key.work_dir))
    }

    pub(super) fn sync_terminal_project(&mut self, cx: &mut Cx) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        let project = project_name(&work_dir);
        let terminal = self.ui.project_terminal(cx, ids!(project_terminal));
        if let Some(group) = self.project_terminals.get(&work_dir) {
            let names = group
                .sessions
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    if index == 0 {
                        project.clone()
                    } else {
                        format!("{project} {}", index + 1)
                    }
                })
                .collect::<Vec<_>>();
            let output = group.sessions.get(group.active).map(Self::terminal_text);
            let output = output
                .as_deref()
                .or(group.error.as_deref())
                .unwrap_or_default();
            terminal.set_terminals(cx, &names, Some(group.active), output);
        } else {
            terminal.set_terminals(cx, &[], None, "");
        }
    }

    fn spawn_project_terminal(
        &mut self,
        work_dir: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<ProjectTerminalSession, String> {
        let pty = Pty::spawn(
            cols,
            rows,
            None,
            &[("TERM", "xterm-256color")],
            Some(work_dir),
        )
        .map_err(|error| format!("Could not start terminal: {error}"))?;
        Ok(ProjectTerminalSession {
            pty,
            emulator: Terminal::new(cols as usize, rows as usize),
        })
    }

    pub(super) fn create_project_terminal(&mut self, cx: &mut Cx) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        if self.project_terminals.get(&work_dir).is_some_and(|group| {
            group.sessions.len() >= crate::components::terminal_panel::MAX_VISIBLE_TERMINALS
        }) {
            return;
        }
        let (cols, rows) = self
            .ui
            .project_terminal(cx, ids!(project_terminal))
            .dimensions(cx)
            .unwrap_or((80, 24));
        match self.spawn_project_terminal(&work_dir, cols as u16, rows as u16) {
            Ok(session) => {
                let group = self.project_terminals.entry(work_dir).or_default();
                group.sessions.push(session);
                group.active = group.sessions.len() - 1;
                group.error = None;
                self.terminal_poll_next_frame = cx.new_next_frame();
            }
            Err(error) => {
                self.project_terminals.entry(work_dir).or_default().error = Some(error);
                self.sync_terminal_project(cx);
                return;
            }
        }
        self.sync_terminal_project(cx);
    }

    pub(super) fn select_project_terminal(&mut self, cx: &mut Cx, index: usize) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        if let Some(group) = self.project_terminals.get_mut(&work_dir) {
            if index < group.sessions.len() {
                group.active = index;
            }
        }
        self.sync_terminal_project(cx);
    }

    pub(super) fn resize_project_terminals(&mut self, cx: &mut Cx, cols: usize, rows: usize) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        let cols = cols.clamp(1, u16::MAX as usize);
        let rows = rows.clamp(1, u16::MAX as usize);
        if let Some(group) = self.project_terminals.get_mut(&work_dir) {
            for session in &mut group.sessions {
                if session.emulator.screen().cols() == cols
                    && session.emulator.screen().rows() == rows
                {
                    continue;
                }
                if let Err(error) = session.pty.resize(cols as u16, rows as u16) {
                    warn!("Terminal resize failed: {error}");
                    continue;
                }
                session.emulator.resize(cols, rows);
            }
        }
        self.sync_terminal_project(cx);
    }

    pub(super) fn close_project_terminal(&mut self, cx: &mut Cx, index: usize) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        let remove_group = if let Some(group) = self.project_terminals.get_mut(&work_dir) {
            if index >= group.sessions.len() {
                return;
            }
            group.sessions.remove(index).terminate();
            if group.sessions.is_empty() {
                true
            } else {
                if group.active >= group.sessions.len() {
                    group.active = group.sessions.len() - 1;
                } else if index < group.active {
                    group.active -= 1;
                }
                false
            }
        } else {
            return;
        };
        if remove_group {
            if let Some(group) = self.project_terminals.remove(&work_dir) {
                group.terminate();
            }
        }
        self.sync_terminal_project(cx);
    }

    pub(super) fn write_terminal_bytes(&mut self, cx: &mut Cx, bytes: Vec<u8>) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        if self
            .project_terminals
            .get(&work_dir)
            .is_none_or(|group| group.sessions.is_empty())
        {
            self.create_project_terminal(cx);
        }
        let Some(group) = self.project_terminals.get_mut(&work_dir) else {
            return;
        };
        let Some(terminal) = group.sessions.get_mut(group.active) else {
            return;
        };
        if let Err(error) = terminal.pty.write(&bytes) {
            warn!("Terminal write failed: {error}");
        }
        self.sync_terminal_project(cx);
    }

    pub(super) fn write_terminal_key(
        &mut self,
        cx: &mut Cx,
        key: TerminalKeyCode,
        shift: bool,
        control: bool,
        alt: bool,
    ) {
        let Some(work_dir) = self.active_terminal_project() else {
            return;
        };
        if self
            .project_terminals
            .get(&work_dir)
            .is_none_or(|group| group.sessions.is_empty())
        {
            self.create_project_terminal(cx);
        }
        let Some(terminal) = self
            .project_terminals
            .get_mut(&work_dir)
            .and_then(|group| group.sessions.get_mut(group.active))
        else {
            return;
        };
        if let Some(bytes) = terminal.emulator.encode_key(key, "", shift, control, alt) {
            if let Err(error) = terminal.pty.write(&bytes) {
                warn!("Terminal write failed: {error}");
            }
        }
        self.sync_terminal_project(cx);
    }

    pub(super) fn poll_terminal_output(&mut self, cx: &mut Cx) {
        const MAX_PTY_READS_PER_FRAME: usize = 8;
        let mut processed_output = false;
        for group in self.project_terminals.values_mut() {
            for session in &mut group.sessions {
                for _ in 0..MAX_PTY_READS_PER_FRAME {
                    let Some(bytes) = session.pty.try_read() else {
                        break;
                    };
                    processed_output = true;
                    session.emulator.process_bytes(&bytes);
                    let outbound = session.emulator.take_outbound();
                    if !outbound.is_empty() {
                        let _ = session.pty.write(&outbound);
                    }
                }
            }
        }
        if processed_output {
            self.sync_terminal_project(cx);
        }
    }

    pub(super) fn has_live_terminal_sessions(&self) -> bool {
        self.project_terminals
            .values()
            .any(|group| !group.sessions.is_empty())
    }

    pub(super) fn terminal_text(session: &ProjectTerminalSession) -> String {
        let screen = session.emulator.screen();
        const CURSOR_MARKER: char = '\u{e000}';
        let cursor_row = screen.scrollback().len() + screen.cursor.y;
        let cursor_col = screen.cursor.x;
        let mut output = String::new();
        let mut push_row = |row_index: usize, cells: &[makepad_terminal_core::Cell]| {
            let mut line: String = cells.iter().map(|cell| cell.codepoint).collect();
            if row_index == cursor_row {
                let width = line.chars().count();
                if width < cursor_col {
                    line.extend(std::iter::repeat_n(' ', cursor_col - width));
                }
                let byte_index = line
                    .char_indices()
                    .nth(cursor_col)
                    .map(|(index, _)| index)
                    .unwrap_or(line.len());
                line.insert(byte_index, CURSOR_MARKER);
            } else {
                line = line.trim_end().to_owned();
            }
            output.push_str(&line);
            output.push('\n');
        };
        for (row, cells) in screen.scrollback().iter().enumerate() {
            push_row(row, cells);
        }
        for row in 0..screen.rows() {
            push_row(screen.scrollback().len() + row, screen.grid.row_slice(row));
        }
        truncate_terminal_output(&mut output);
        output.pop();
        output
    }
}
