use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use gpui::*;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, ElementExt};
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
                log::warn!("failed to write to terminal PTY: {error}");
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
            log::warn!("failed to resize terminal PTY: {error}");
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
    screen_bounds: Option<Bounds<Pixels>>,
    selection_anchor: Option<(u16, u16)>,
    selection_head: Option<(u16, u16)>,
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
            screen_bounds: None,
            selection_anchor: None,
            selection_head: None,
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
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.session.take();
        self.parser = vt100::Parser::new(self.rows, self.cols, SCROLLBACK_ROWS);
        self.status = None;
        self.start();
        cx.notify();
    }

    /// Clears both the emulator scrollback and the visible screen.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
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

    fn paste(&self, text: String) {
        if self.parser.screen().bracketed_paste() {
            self.send(b"\x1b[200~");
            self.send(text.as_bytes());
            self.send(b"\x1b[201~");
        } else {
            self.send(text.as_bytes());
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.paste(text);
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
                    .map(|text| {
                        if self.parser.screen().bracketed_paste() {
                            [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~".as_slice()].concat()
                        } else {
                            text.into_bytes()
                        }
                    }),
                _ => None,
            }
        } else {
            match key {
                "enter" => Some(b"\r".to_vec()),
                "backspace" => Some(vec![0x7f]),
                "tab" => Some(b"\t".to_vec()),
                "up" => Some(self.cursor_key(b'A', modifiers)),
                "down" => Some(self.cursor_key(b'B', modifiers)),
                "right" => Some(self.cursor_key(b'C', modifiers)),
                "left" => Some(self.cursor_key(b'D', modifiers)),
                "home" => Some(b"\x1b[H".to_vec()),
                "end" => Some(b"\x1b[F".to_vec()),
                "delete" => Some(b"\x1b[3~".to_vec()),
                "pageup" => Some(b"\x1b[5~".to_vec()),
                "pagedown" => Some(b"\x1b[6~".to_vec()),
                "f1" => Some(b"\x1bOP".to_vec()),
                "f2" => Some(b"\x1bOQ".to_vec()),
                "f3" => Some(b"\x1bOR".to_vec()),
                "f4" => Some(b"\x1bOS".to_vec()),
                "f5" => Some(b"\x1b[15~".to_vec()),
                "f6" => Some(b"\x1b[17~".to_vec()),
                "f7" => Some(b"\x1b[18~".to_vec()),
                "f8" => Some(b"\x1b[19~".to_vec()),
                "f9" => Some(b"\x1b[20~".to_vec()),
                "f10" => Some(b"\x1b[21~".to_vec()),
                "f11" => Some(b"\x1b[23~".to_vec()),
                "f12" => Some(b"\x1b[24~".to_vec()),
                "escape" => Some(vec![0x1b]),
                _ if !modifiers.platform => event
                    .keystroke
                    .key_char
                    .as_ref()
                    .map(|text| {
                        let mut bytes = Vec::with_capacity(text.len() + usize::from(modifiers.alt));
                        if modifiers.alt {
                            bytes.push(0x1b);
                        }
                        bytes.extend_from_slice(text.as_bytes());
                        bytes
                    }),
                _ => None,
            }
        };

        if let Some(bytes) = bytes {
            self.send(&bytes);
            cx.stop_propagation();
        }
    }

    fn cursor_key(&self, key: u8, modifiers: Modifiers) -> Vec<u8> {
        let modifier = 1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt)
            + 4 * u8::from(modifiers.control);
        if modifier == 1 {
            if self.parser.screen().application_cursor() {
                vec![0x1b, b'O', key]
            } else {
                vec![0x1b, b'[', key]
            }
        } else {
            format!("\x1b[1;{modifier}{}", key as char).into_bytes()
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

    fn cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let bounds = self.screen_bounds?;
        let x = ((position.x - bounds.left()).as_f32() - 12.0) / 7.8;
        let y = ((position.y - bounds.top()).as_f32() - 12.0) / 17.55;
        Some((
            y.floor().max(0.0).min(f32::from(self.rows.saturating_sub(1))) as u16,
            x.floor().max(0.0).min(f32::from(self.cols.saturating_sub(1))) as u16,
        ))
    }

    fn selected_text(&self) -> Option<String> {
        let (anchor, head) = (self.selection_anchor?, self.selection_head?);
        if anchor == head {
            return None;
        }
        let (start, end) = if anchor <= head { (anchor, head) } else { (head, anchor) };
        Some(self.parser.screen().contents_between(start.0, start.1, end.0, end.1))
    }

    fn begin_selection(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_anchor = self.cell_at(event.position);
        self.selection_head = self.selection_anchor;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn extend_selection(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.dragging() && self.selection_anchor.is_some() {
            self.selection_head = self.cell_at(event.position);
            cx.notify();
        }
    }

    fn end_selection(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.selection_head = self.cell_at(event.position).or(self.selection_head);
        cx.notify();
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
        let terminal_text = self.screen_text();
        let selected_text = self.selected_text();
        let terminal_resize = cx.entity().clone();
        let terminal_actions = cx.entity().clone();

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
                    .id("pty-terminal-screen")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_3()
                    .font_family(".ZedMono")
                    .text_size(px(13.0))
                    .line_height(relative(1.35))
                    .cursor_text()
                    .on_prepaint(move |bounds, _, cx| {
                        let rows = ((bounds.size.height.as_f32() - 24.0) / 17.55).floor() as u16;
                        let cols = ((bounds.size.width.as_f32() - 24.0) / 7.8).floor() as u16;
                        terminal_resize.update(cx, |terminal, cx| {
                            terminal.screen_bounds = Some(bounds);
                            terminal.resize(rows, cols, cx);
                        });
                    })
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_selection))
                    .on_mouse_move(cx.listener(Self::extend_selection))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::end_selection))
                    .child(terminal_text.clone())
                    .context_menu({
                        let text = terminal_text.clone();
                        let selection = selected_text.clone();
                        let clear_terminal = terminal_actions.clone();
                        let restart_terminal = terminal_actions.clone();
                        move |menu, _window, _cx| {
                            let output = text.clone();
                            let menu = if let Some(selection) = &selection {
                                let selection = selection.clone();
                                menu.item(PopupMenuItem::new("Copy Selection").on_click(
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(selection.clone()));
                                    },
                                ))
                            } else {
                                menu
                            };
                            let clear_terminal = clear_terminal.clone();
                            let restart_terminal = restart_terminal.clone();
                            menu.item(PopupMenuItem::new("Copy Terminal Output").on_click(
                                move |_event, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(output.clone()));
                                },
                            ))
                            .item(PopupMenuItem::new("Clear Terminal").on_click(
                                move |_event, _window, cx| {
                                    clear_terminal.update(cx, |terminal, cx| terminal.clear(cx));
                                },
                            ))
                            .item(PopupMenuItem::new("Restart Terminal").on_click(
                                move |_event, _window, cx| {
                                    restart_terminal.update(cx, |terminal, cx| terminal.restart(cx));
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
