use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Sizable};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

const DEFAULT_ROWS: u16 = 30;
const DEFAULT_COLS: u16 = 120;
const SCROLLBACK_ROWS: usize = 10_000;

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            if let Err(error) = writer.write_all(bytes).and_then(|_| writer.flush()) {
                tracing::warn!("failed to write to terminal PTY: {error}");
            }
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        if let Err(error) = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            tracing::warn!("failed to resize terminal PTY: {error}");
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

enum PtyEvent {
    Output(Vec<u8>),
    Closed,
    Error(String),
}

/// A persistent, focusable project shell backed by a real pseudo-terminal.
///
/// Construct it with `cx.new(|cx| TerminalView::new(project, cx))` and
/// render the resulting `Entity<TerminalView>` directly from its parent view.
pub struct TerminalView {
    project: PathBuf,
    focus_handle: FocusHandle,
    parser: vt100::Parser,
    session: Option<PtySession>,
    event_tx: mpsc::Sender<PtyEvent>,
    status: Option<String>,
    rows: u16,
    cols: u16,
}

impl TerminalView {
    pub(crate) fn new(project: PathBuf, cx: &mut Context<Self>) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            let events = event_rx.try_iter().collect::<Vec<_>>();
            if events.is_empty() {
                continue;
            }
            let _ = this.update(cx, |this, cx| {
                for event in events {
                    this.apply_event(event);
                }
                cx.notify();
            });
        })
        .detach();

        let mut terminal = Self {
            project,
            focus_handle: cx.focus_handle(),
            parser: vt100::Parser::new(DEFAULT_ROWS, DEFAULT_COLS, SCROLLBACK_ROWS),
            session: None,
            event_tx,
            status: None,
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
        };
        terminal.start();
        terminal
    }

    /// Switches the shell to another project, restarting it in the new cwd.
    pub fn set_project(&mut self, project: PathBuf, cx: &mut Context<Self>) {
        if self.project != project {
            self.project = project;
            self.restart(cx);
        }
    }

    /// Terminates the current shell and starts a fresh login-capable interactive shell.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.session.take();
        self.parser = vt100::Parser::new(self.rows, self.cols, SCROLLBACK_ROWS);
        self.status = None;
        self.start();
        cx.notify();
    }

    /// Clears both the emulator scrollback and the visible screen.
    fn clear(&mut self, cx: &mut Context<Self>) {
        self.parser = vt100::Parser::new(self.rows, self.cols, SCROLLBACK_ROWS);
        self.status = None;
        cx.notify();
    }

    /// Updates the PTY and terminal parser dimensions. Parents can call this when
    /// they have measured cell dimensions for their allocated terminal bounds.
    pub fn resize(&mut self, rows: u16, cols: u16, cx: &mut Context<Self>) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if (rows, cols) == (self.rows, self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.parser.set_size(rows, cols);
        if let Some(session) = &self.session {
            session.resize(rows, cols);
        }
        cx.notify();
    }

    pub fn project(&self) -> &PathBuf {
        &self.project
    }

    fn start(&mut self) {
        match spawn_shell(&self.project, self.rows, self.cols, self.event_tx.clone()) {
            Ok(session) => self.session = Some(session),
            Err(error) => {
                self.session = None;
                self.status = Some(format!("Unable to start terminal: {error}"));
            }
        }
    }

    fn apply_event(&mut self, event: PtyEvent) {
        match event {
            PtyEvent::Output(bytes) => {
                self.parser.process(&bytes);
                self.status = None;
            }
            PtyEvent::Closed => {
                if self.session.is_some() {
                    self.status = Some("Shell exited. Select Restart to open a new shell.".into());
                }
            }
            PtyEvent::Error(error) => self.status = Some(format!("Terminal read failed: {error}")),
        }
    }

    fn send(&self, bytes: &[u8]) {
        if let Some(session) = &self.session {
            session.write(bytes);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.send(text.as_bytes());
            }
            cx.stop_propagation();
            return;
        }

        let bytes: Option<Vec<u8>> = if modifiers.control {
            match key.to_ascii_lowercase().as_str() {
                "c" => Some(vec![0x03]),
                "d" => Some(vec![0x04]),
                "l" => Some(vec![0x0c]),
                "z" => Some(vec![0x1a]),
                "v" if !modifiers.platform => cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .map(|text| text.into_bytes()),
                _ => None,
            }
        } else {
            match key {
                "enter" => Some(b"\r".to_vec()),
                "backspace" => Some(vec![0x7f]),
                "tab" => Some(b"\t".to_vec()),
                "up" => Some(b"\x1b[A".to_vec()),
                "down" => Some(b"\x1b[B".to_vec()),
                "right" => Some(b"\x1b[C".to_vec()),
                "left" => Some(b"\x1b[D".to_vec()),
                "home" => Some(b"\x1b[H".to_vec()),
                "end" => Some(b"\x1b[F".to_vec()),
                "delete" => Some(b"\x1b[3~".to_vec()),
                "escape" => Some(vec![0x1b]),
                _ if !modifiers.platform && !modifiers.alt => event
                    .keystroke
                    .key_char
                    .as_ref()
                    .map(|text| text.as_bytes().to_vec()),
                _ => None,
            }
        };

        if let Some(bytes) = bytes {
            self.send(&bytes);
            cx.stop_propagation();
        }
    }

    fn screen_text(&self) -> String {
        let mut text = self.parser.screen().contents();
        if let Some(status) = &self.status {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(status);
        }
        text
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let focus_handle = self.focus_handle.clone();
        let project_name = self
            .project
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.project.display().to_string());
        let terminal_text = self.screen_text();

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .child(
                div()
                    .h(px(38.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().truncate().text_xs().child(project_name))
                    .child(
                        Button::new("pty-terminal-clear")
                            .label("Clear")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                    )
                    .child(
                        Button::new("pty-terminal-restart")
                            .label("Restart")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| this.restart(cx))),
                    ),
            )
            .child(
                div()
                    .id("pty-terminal-screen")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_3()
                    .font_family(".ZedMono")
                    .text_size(px(13.0))
                    .line_height(relative(1.35))
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        focus_handle.focus(window, cx);
                    })
                    .child(terminal_text.clone())
                    .context_menu({
                        let text = terminal_text.clone();
                        move |menu, _window, _cx| {
                            let output = text.clone();
                            menu.item(PopupMenuItem::new("Copy Terminal Output").on_click(
                                move |_event, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        output.clone(),
                                    ));
                                },
                            ))
                        }
                    }),
            )
    }
}

fn spawn_shell(
    project: &PathBuf,
    rows: u16,
    cols: u16,
    event_tx: mpsc::Sender<PtyEvent>,
) -> anyhow::Result<PtySession> {
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut command = CommandBuilder::new(shell);
    command.cwd(project);
    command.env("TERM", "xterm-256color");
    command.arg("-i");

    let child = pair.slave.spawn_command(command)?;
    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    drop(pair.slave);

    std::thread::Builder::new()
        .name("threadlane-gpui-pty-reader".into())
        .spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = event_tx.send(PtyEvent::Closed);
                        break;
                    }
                    Ok(read) => {
                        if event_tx
                            .send(PtyEvent::Output(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = event_tx.send(PtyEvent::Error(error.to_string()));
                        break;
                    }
                }
            }
        })?;

    Ok(PtySession {
        master: pair.master,
        writer,
        child,
    })
}
