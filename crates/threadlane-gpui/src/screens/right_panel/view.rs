use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};

use gpui_component::scroll::ScrollableElement;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Icon, IconName, Selectable, Sizable};
use threadlane_git::GitFile;

use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    Review,
    Files,
    Terminal,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Files => "Files",
            Self::Terminal => "Terminal",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Review => "icons/file.svg",
            Self::Files => "icons/folder.svg",
            Self::Terminal => "icons/square-terminal.svg",
        }
    }
}

#[derive(Clone, Debug)]
struct FileEntry {
    relative_path: String,
    name: String,
    is_dir: bool,
    depth: usize,
}

enum PanelEvent {
    FilesLoaded {
        project: PathBuf,
        entries: Vec<FileEntry>,
    },
    ReviewLoaded {
        project: PathBuf,
        files: Vec<GitFile>,
        error: Option<String>,
    },
    DocumentLoaded {
        project: PathBuf,
        title: String,
        content: String,
    },
    CommandFinished {
        project: PathBuf,
        command: String,
        output: String,
    },
}

pub struct RightPanelView {
    model: Entity<AppState>,
    active_surface: Option<Surface>,
    project: Option<PathBuf>,
    files: Vec<FileEntry>,
    review_files: Vec<GitFile>,
    review_error: Option<String>,
    document_title: Option<String>,
    document_state: Entity<TextViewState>,
    terminal_input: Entity<InputState>,
    terminal_state: Entity<TextViewState>,
    terminal_output: String,
    event_tx: mpsc::Sender<PanelEvent>,
    _subscriptions: Vec<Subscription>,
}

impl RightPanelView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal_input = cx.new(|cx| InputState::new(window, cx).placeholder("Run a command…"));
        let document_state = cx.new(|cx| TextViewState::markdown("", cx));
        let terminal_state = cx.new(|cx| TextViewState::markdown("", cx));
        let (event_tx, event_rx) = mpsc::channel();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let events = event_rx.try_iter().collect::<Vec<_>>();
            if events.is_empty() {
                continue;
            }
            let _ = this.update(cx, |this, cx| {
                for event in events {
                    this.apply_event(event, cx);
                }
                cx.notify();
            });
        })
        .detach();

        let command_view = cx.entity().downgrade();
        let command_subscription = cx.subscribe_in(
            &terminal_input,
            window,
            move |_this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let command = input.read(cx).value().trim().to_string();
                if command.is_empty() {
                    return;
                }
                input.update(cx, |input, cx| input.set_value("", window, cx));
                let _ = command_view.update(cx, |this, cx| {
                    this.run_command(command, cx);
                });
            },
        );
        let observe_model = cx.observe(&model, |_this, _model, cx| cx.notify());

        Self {
            model,
            active_surface: None,
            project: None,
            files: Vec::new(),
            review_files: Vec::new(),
            review_error: None,
            document_title: None,
            document_state,
            terminal_input,
            terminal_state,
            terminal_output: String::new(),
            event_tx,
            _subscriptions: vec![command_subscription, observe_model],
        }
    }

    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let project = self.model.read(cx).active_work_dir.clone();
        if self.project == project {
            return;
        }
        self.project = project;
        self.files.clear();
        self.review_files.clear();
        self.review_error = None;
        self.document_title = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        self.terminal_output.clear();
        self.sync_terminal_text(cx);
        self.refresh_active_surface();
    }

    fn open_surface(&mut self, surface: Surface, cx: &mut Context<Self>) {
        if self.active_surface != Some(surface) {
            self.document_title = None;
            self.document_state
                .update(cx, |state, cx| state.set_text("", cx));
        }
        self.active_surface = Some(surface);
        self.refresh_surface(surface);
        cx.notify();
    }

    fn refresh_active_surface(&mut self) {
        if let Some(surface) = self.active_surface {
            self.refresh_surface(surface);
        }
    }

    fn refresh_surface(&self, surface: Surface) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        std::thread::spawn(move || match surface {
            Surface::Files => {
                let entries = scan_project_files(&project, 500);
                let _ = tx.send(PanelEvent::FilesLoaded { project, entries });
            }
            Surface::Review => {
                let (files, error) = match threadlane_git::inspect(&project) {
                    Ok(status) => (status.files, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                let _ = tx.send(PanelEvent::ReviewLoaded {
                    project,
                    files,
                    error,
                });
            }
            Surface::Terminal => {}
        });
    }

    fn open_file(&self, relative_path: String) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let content = std::fs::read_to_string(project.join(&relative_path))
                .unwrap_or_else(|error| format!("Unable to open {relative_path}: {error}"));
            let _ = tx.send(PanelEvent::DocumentLoaded {
                project,
                title: relative_path,
                content,
            });
        });
    }

    fn close_document(&mut self, cx: &mut Context<Self>) {
        self.document_title = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        cx.notify();
    }

    fn open_diff(&self, relative_path: String) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let content = threadlane_git::diff_file(&project, &relative_path)
                .unwrap_or_else(|error| error.to_string());
            let _ = tx.send(PanelEvent::DocumentLoaded {
                project,
                title: format!("Review · {relative_path}"),
                content,
            });
        });
    }

    fn run_command(&mut self, command: String, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        self.terminal_output.push_str(&format!("\n$ {command}\n"));
        self.sync_terminal_text(cx);
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let output = Command::new("sh")
                .arg("-lc")
                .arg(&command)
                .current_dir(&project)
                .output()
                .map(|output| {
                    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&output.stderr));
                    text
                })
                .unwrap_or_else(|error| format!("Unable to run command: {error}\n"));
            let _ = tx.send(PanelEvent::CommandFinished {
                project,
                command,
                output,
            });
        });
    }

    fn apply_event(&mut self, event: PanelEvent, cx: &mut Context<Self>) {
        match event {
            PanelEvent::FilesLoaded { project, entries }
                if self.project.as_ref() == Some(&project) =>
            {
                self.files = entries;
            }
            PanelEvent::ReviewLoaded {
                project,
                files,
                error,
            } if self.project.as_ref() == Some(&project) => {
                self.review_files = files;
                self.review_error = error;
            }
            PanelEvent::DocumentLoaded {
                project,
                title,
                content,
            } if self.project.as_ref() == Some(&project) => {
                self.document_title = Some(title);
                let markdown = format!("```diff\n{}\n```", content.replace("```", "` ` `"));
                self.document_state
                    .update(cx, |state, cx| state.set_text(&markdown, cx));
            }
            PanelEvent::CommandFinished {
                project,
                command,
                output,
            } if self.project.as_ref() == Some(&project) => {
                if output.is_empty() {
                    self.terminal_output
                        .push_str(&format!("{command} completed.\n"));
                } else {
                    self.terminal_output.push_str(&output);
                    if !output.ends_with('\n') {
                        self.terminal_output.push('\n');
                    }
                }
                self.sync_terminal_text(cx);
            }
            _ => {}
        }
    }

    fn sync_terminal_text(&self, cx: &mut Context<Self>) {
        let text = if self.terminal_output.is_empty() {
            "Run project commands here. This command console is not a persistent PTY.".to_string()
        } else {
            format!(
                "```text\n{}\n```",
                self.terminal_output.replace("```", "` ` `")
            )
        };
        self.terminal_state
            .update(cx, |state, cx| state.set_text(&text, cx));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let active = self.active_surface;
        div()
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .children(
                [Surface::Review, Surface::Files, Surface::Terminal].map(|surface| {
                    Button::new(SharedString::from(format!(
                        "right-panel-tab-{}",
                        surface.label().to_lowercase()
                    )))
                    .icon(Icon::default().path(surface.icon()))
                    .label(surface.label())
                    .ghost()
                    .selected(active == Some(surface))
                    .small()
                    .on_click(cx.listener(
                        move |this, _event, _window, cx| {
                            this.open_surface(surface, cx);
                        },
                    ))
                }),
            )
            .child(div().flex_1())
            .child(
                Button::new("right-panel-refresh")
                    .icon(Icon::default().path("icons/redo.svg"))
                    .tooltip("Refresh surface")
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.refresh_active_surface();
                        cx.notify();
                    })),
            )
    }

    fn render_chooser(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Open a surface"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Choose what to show in the right panel"),
                    )
                    .child(div().mt_4().w_full().flex().gap_2().children(
                        [Surface::Review, Surface::Files, Surface::Terminal].map(|surface| {
                            Button::new(SharedString::from(format!(
                                "right-panel-card-{}",
                                surface.label().to_lowercase()
                            )))
                            .child(
                                div()
                                    .size_full()
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .justify_center()
                                    .gap_2()
                                    .text_sm()
                                    .child(Icon::default().path(surface.icon()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(surface.label()),
                                    ),
                            )
                            .outline()
                            .flex_1()
                            .h(px(104.0))
                            .p_0()
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    this.open_surface(surface, cx);
                                },
                            ))
                        }),
                    )),
            )
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if let Some(title) = &self.document_title {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(38.0))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            Button::new("right-panel-document-back")
                                .icon(IconName::ArrowLeft)
                                .tooltip(match self.active_surface {
                                    Some(Surface::Review) => "Back to changed files",
                                    _ => "Back to project files",
                                })
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.close_document(cx);
                                })),
                        )
                        .child(Icon::default().path("icons/file.svg"))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_xs()
                                .child(title.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_3()
                        .child(TextView::new(&self.document_state).selectable(true)),
                )
                .into_any_element();
        }
        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .py_2()
            .children(self.files.iter().cloned().map(|entry| {
                let relative_path = entry.relative_path.clone();
                div()
                    .id(SharedString::from(format!("project-file-{relative_path}")))
                    .h(px(30.0))
                    .mx_2()
                    .pl(px(8.0 + entry.depth as f32 * 14.0))
                    .pr_2()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .hover(|row| row.bg(theme.muted))
                    .child(Icon::default().path(if entry.is_dir {
                        "icons/folder.svg"
                    } else {
                        "icons/file.svg"
                    }))
                    .child(entry.name)
                    .when(!entry.is_dir, |row| {
                        row.cursor_pointer().on_click(cx.listener(
                            move |this, _event, _window, _cx| {
                                this.open_file(relative_path.clone());
                            },
                        ))
                    })
            }))
            .into_any_element()
    }

    fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if self
            .document_title
            .as_deref()
            .is_some_and(|title| title.starts_with("Review ·"))
        {
            return self.render_files(cx);
        }
        if let Some(error) = &self.review_error {
            return self.render_empty("Review unavailable", error, cx);
        }
        if self.review_files.is_empty() {
            return self.render_empty("No changes", "The working tree is clean.", cx);
        }
        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .py_2()
            .children(self.review_files.iter().cloned().map(|file| {
                let path = file.path.clone();
                let status = file.status_char().to_string();
                div()
                    .id(SharedString::from(format!("review-file-{path}")))
                    .h(px(36.0))
                    .mx_2()
                    .px_2()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.muted))
                    .child(Icon::default().path("icons/file.svg"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .child(file.path),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.success)
                            .child(format!("+{}", file.additions)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.danger)
                            .child(format!("-{}", file.deletions)),
                    )
                    .child(
                        div()
                            .w(px(18.0))
                            .text_center()
                            .text_xs()
                            .text_color(theme.warning)
                            .child(status),
                    )
                    .on_click(cx.listener(move |this, _event, _window, _cx| {
                        this.open_diff(path.clone());
                    }))
            }))
            .into_any_element()
    }

    fn render_terminal(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let project_name = self
            .project
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No project".into());
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .h(px(38.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::default().path("icons/square-terminal.svg"))
                    .child(div().flex_1().text_xs().child(project_name))
                    .child(
                        Button::new("terminal-clear")
                            .label("Clear")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.terminal_output.clear();
                                this.sync_terminal_text(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_3()
                    .child(TextView::new(&self.terminal_state).selectable(true)),
            )
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Input::new(&self.terminal_input).prefix("$")),
            )
            .into_any_element()
    }

    fn render_empty(&self, title: &str, description: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_string()),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(description.to_string()),
            )
            .into_any_element()
    }
}

impl Render for RightPanelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_project(cx);
        let theme = cx.theme().colors;
        let body = match self.active_surface {
            None => self.render_chooser(cx).into_any_element(),
            Some(Surface::Review) => self.render_review(cx),
            Some(Surface::Files) => self.render_files(cx),
            Some(Surface::Terminal) => self.render_terminal(cx),
        };
        div()
            .w_full()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(body)
    }
}

fn scan_project_files(root: &Path, limit: usize) -> Vec<FileEntry> {
    fn visit(root: &Path, relative: &Path, depth: usize, limit: usize, rows: &mut Vec<FileEntry>) {
        if rows.len() >= limit || depth > 5 {
            return;
        }
        let Ok(read_dir) = std::fs::read_dir(root.join(relative)) else {
            return;
        };
        let mut children = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == "target" || name == ".threadlane" {
                    return None;
                }
                Some((name, entry.file_type().ok()?.is_dir()))
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(name, is_dir)| (!*is_dir, name.to_ascii_lowercase()));
        for (name, is_dir) in children {
            if rows.len() >= limit {
                break;
            }
            let path = relative.join(&name);
            rows.push(FileEntry {
                relative_path: path.to_string_lossy().into_owned(),
                name,
                is_dir,
                depth,
            });
            if is_dir {
                visit(root, &path, depth + 1, limit, rows);
            }
        }
    }

    let mut rows = Vec::new();
    visit(root, Path::new(""), 0, limit, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::scan_project_files;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn project_scan_is_bounded_and_skips_generated_roots() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("threadlane-panel-{nonce}"));
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join(".threadlane/sessions")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "pub fn value() {}\n").unwrap();
        std::fs::write(root.join("target/debug/generated"), "ignored").unwrap();

        let rows = scan_project_files(&root, 10);
        assert!(rows.len() <= 10);
        assert!(rows
            .iter()
            .any(|entry| entry.relative_path == "src/main.rs"));
        assert!(!rows
            .iter()
            .any(|entry| entry.relative_path.starts_with("target")));
        assert!(!rows
            .iter()
            .any(|entry| entry.relative_path.starts_with(".threadlane")));

        std::fs::remove_dir_all(root).unwrap();
    }
}
