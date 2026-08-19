use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::separator::Separator;
use gpui_component::tag::Tag;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Disableable, IconName, Selectable, Sizable};
use threadlane_git::GitFile;

use crate::state::AppState;


fn detect_language(path_str: &str) -> &'static str {
    let path = Path::new(path_str);
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts" | "mts" | "cts" | "jsx" | "tsx") => "typescript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("md" | "markdown") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc" | "cxx" | "hh") => "cpp",
        Some("diff" | "patch") => "diff",
        Some("zig") => "zig",
        _ => match path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("dockerfile") => "bash",
            Some("cargo.lock") => "toml",
            _ => "text",
        },
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    Review,
    Files,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Files => "Files",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Review => IconName::File,
            Self::Files => IconName::Folder,
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
}

pub struct RightPanelView {
    model: Entity<AppState>,
    active_surface: Option<Surface>,
    project: Option<PathBuf>,
    files: Vec<FileEntry>,
    expanded_paths: HashSet<String>,
    review_files: Vec<GitFile>,
    review_error: Option<String>,
    document_title: Option<String>,
    document_state: Entity<TextViewState>,
    editor_state: Option<Entity<EditorState>>,
    editor_subscription: Option<Subscription>,
    saved_content: String,
    is_dirty: bool,
    pending_document: Option<(String, String)>,
    event_tx: mpsc::Sender<PanelEvent>,
    _subscriptions: Vec<Subscription>,
}

impl RightPanelView {
    pub(crate) fn new(
        model: Entity<AppState>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_state = cx.new(|cx| TextViewState::markdown("", cx));
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

        let observe_model = cx.observe(&model, |_this, _model, cx| cx.notify());

        Self {
            model,
            active_surface: None,
            project: None,
            files: Vec::new(),
            expanded_paths: HashSet::new(),
            review_files: Vec::new(),
            review_error: None,
            document_title: None,
            document_state,
            editor_state: None,
            editor_subscription: None,
            saved_content: String::new(),
            is_dirty: false,
            pending_document: None,
            event_tx,
            _subscriptions: vec![observe_model],
        }
    }

    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let project = self.model.read(cx).active_work_dir.clone();
        if self.project == project {
            return;
        }
        self.project = project;
        self.files.clear();
        self.expanded_paths.clear();
        self.review_files.clear();
        self.review_error = None;
        self.document_title = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        self.refresh_active_surface();
    }

    pub(crate) fn open_review(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
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
        let expanded_paths = self.expanded_paths.clone();
        std::thread::spawn(move || match surface {
            Surface::Files => {
                let entries = scan_project_files(&project, &expanded_paths, 500);
                let _ = tx.send(PanelEvent::FilesLoaded { project, entries });
            }
            Surface::Review => {
                let (files, error) = match threadlane_git::inspect_files(&project) {
                    Ok(files) => (files, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                let _ = tx.send(PanelEvent::ReviewLoaded {
                    project,
                    files,
                    error,
                });
            }
        });
    }

    fn toggle_folder(&mut self, relative_path: String, cx: &mut Context<Self>) {
        if !self.expanded_paths.remove(&relative_path) {
            self.expanded_paths.insert(relative_path);
        }
        self.refresh_surface(Surface::Files);
        cx.notify();
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
        self.editor_state = None;
        self.editor_subscription = None;
        self.saved_content.clear();
        self.is_dirty = false;
        self.pending_document = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        cx.notify();
    }

    fn sync_pending_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((title, content)) = self.pending_document.take() else {
            return;
        };
        self.document_title = Some(title.clone());
        self.saved_content = content.clone();
        self.is_dirty = false;

        if title.starts_with("Review ·") {
            self.editor_state = None;
            self.editor_subscription = None;
            let markdown = format!("```diff\n{}\n```", content.replace("```", "` ` `"));
            self.document_state
                .update(cx, |state, cx| state.set_text(&markdown, cx));
        } else {
            let lang = detect_language(&title);
            let editor = cx.new(|cx| {
                EditorState::new(lang, window, cx)
                    .line_number(true)
                    .folding(true)
                    .show_whitespaces(false)
                    .tab_size(TabSize {
                        tab_size: 4,
                        hard_tabs: false,
                    })
                    .default_value(&content)
            });
            let subscription = cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let current = editor.read(cx).value();
                    let dirty = current.as_str() != this.saved_content.as_str();
                    if this.is_dirty != dirty {
                        this.is_dirty = dirty;
                        cx.notify();
                    }
                }
            });
            self.editor_state = Some(editor);
            self.editor_subscription = Some(subscription);
        }
    }

    fn save_active_document(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor_state.as_ref() else {
            return;
        };
        let Some(title) = self.document_title.as_ref() else {
            return;
        };
        let Some(project) = self.project.as_ref() else {
            return;
        };

        let content = editor.read(cx).value().to_string();
        let file_path = project.join(title);

        if let Err(err) = std::fs::write(&file_path, &content) {
            log::error!("Failed to save file {}: {}", file_path.display(), err);
        } else {
            self.saved_content = content;
            self.is_dirty = false;
            cx.notify();
        }
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

    fn apply_event(&mut self, event: PanelEvent, _cx: &mut Context<Self>) {
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
                self.pending_document = Some((title, content));
            }
            _ => {}
        }
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
                [Surface::Review, Surface::Files].map(|surface| {
                    Button::new(SharedString::from(format!(
                        "right-panel-tab-{}",
                        surface.label().to_lowercase()
                    )))
                    .icon(surface.icon())
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
                    .icon(IconName::Redo)
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
                        [Surface::Review, Surface::Files].map(|surface| {
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
                                    .child(surface.icon())
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
        let project = self.project.clone();
        if let Some(title) = &self.document_title {
            let is_dirty = self.is_dirty;
            let has_editor = self.editor_state.is_some();
            let lang = detect_language(title);
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
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .min_w_0()
                                .flex_1()
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
                                .child(IconName::File)
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(title.clone()),
                                )
                                .children(is_dirty.then(|| {
                                    Tag::warning()
                                        .child("modified")
                                        .xsmall()
                                }))
                                .children(has_editor.then(|| {
                                    Tag::secondary()
                                        .child(lang)
                                        .outline()
                                        .xsmall()
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .children(has_editor.then(|| {
                                    Button::new("save-document")
                                        .small()
                                        .label("Save")
                                        .icon(IconName::Check)
                                        .disabled(!is_dirty)
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.save_active_document(cx);
                                        }))
                                }))
                                .child(
                                    Button::new("close-document")
                                        .small()
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.close_document(cx);
                                        })),
                                ),
                        ),
                )
                .child(Separator::horizontal())
                .child(if let Some(ref editor) = self.editor_state {
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .h_full()
                        .child(Editor::new(editor).bordered(false).size_full())
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_3()
                        .child(TextView::new(&self.document_state).selectable(true))
                        .into_any_element()
                })
                .into_any_element();
        }
        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .py_2()
            .children(self.files.iter().cloned().map(|entry| {
                let relative_path = entry.relative_path.clone();
                let context_path = relative_path.clone();
                let absolute_path = project
                    .as_ref()
                    .map(|root| root.join(&relative_path).display().to_string());
                let folder_path = relative_path.clone();
                let file_path = relative_path.clone();
                let expanded = self.expanded_paths.contains(&relative_path);
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
                    .child(if entry.is_dir {
                        if expanded {
                            IconName::ChevronDown.into_any_element()
                        } else {
                            IconName::ChevronRight.into_any_element()
                        }
                    } else {
                        div().w(px(16.0)).flex_none().into_any_element()
                    })
                    .child(if entry.is_dir {
                        IconName::Folder.into_any_element()
                    } else {
                        IconName::File.into_any_element()
                    })
                    .child(entry.name)
                    .when(entry.is_dir, |row| {
                        row.cursor_pointer().on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.toggle_folder(folder_path.clone(), cx);
                            },
                        ))
                    })
                    .when(!entry.is_dir, |row| {
                        let target_path = file_path.clone();
                        row.cursor_pointer().on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.model.update(cx, |state, cx| {
                                    state.request_open_file(target_path.clone());
                                    cx.notify();
                                });
                            },
                        ))
                    })
                    .context_menu({
                        let path = context_path.clone();
                        let absolute_path = absolute_path.clone();
                        let editor_path = context_path.clone();
                        let model = self.model.clone();
                        move |menu, _window, _cx| {
                            let text = path.clone();
                            let ed_path = editor_path.clone();
                            let model_ref = model.clone();
                            let menu = menu.item(
                                PopupMenuItem::new("Open in Editor Tab").on_click(
                                    move |_event, _window, cx| {
                                        model_ref.update(cx, |state, cx| {
                                            state.request_open_file(ed_path.clone());
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                            let menu =
                                menu.item(PopupMenuItem::new("Copy Relative Path").on_click(
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ));
                                    },
                                ));
                            if let Some(absolute_path) = absolute_path.clone() {
                                let text = absolute_path;
                                menu.item(PopupMenuItem::new("Copy Absolute Path").on_click(
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ));
                                    },
                                ))
                            } else {
                                menu
                            }
                        }
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
        let total_additions: u32 = self.review_files.iter().map(|f| f.additions).sum();
        let total_deletions: u32 = self.review_files.iter().map(|f| f.deletions).sum();
        let summary_header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!(
                        "{} changed {}",
                        self.review_files.len(),
                        if self.review_files.len() == 1 {
                            "file"
                        } else {
                            "files"
                        }
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.success)
                            .child(format!("+{total_additions}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.danger)
                            .child(format!("-{total_deletions}")),
                    ),
            );

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(summary_header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .py_2()
                    .children(self.review_files.iter().cloned().map(|file| {
                        let path = file.path.clone();
                        let absolute_path = self
                            .project
                            .as_ref()
                            .map(|root| root.join(&path).display().to_string());
                        let status = file.status_char().to_string();
                        let context_path = path.clone();
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
                            .child(IconName::File)
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
                            .context_menu({
                                let path = context_path.clone();
                                let absolute_path = absolute_path.clone();
                                move |menu, _window, _cx| {
                                    let text = path.clone();
                                    let menu = menu.item(
                                        PopupMenuItem::new("Copy Relative Path").on_click(
                                            move |_event, _window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text.clone(),
                                                ));
                                            },
                                        ),
                                    );
                                    if let Some(absolute_path) = absolute_path.clone() {
                                        let text = absolute_path;
                                        menu.item(
                                            PopupMenuItem::new("Copy Absolute Path").on_click(
                                                move |_event, _window, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(text.clone()),
                                                    );
                                                },
                                            ),
                                        )
                                    } else {
                                        menu
                                    }
                                }
                            })
                    })),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_project(cx);
        self.sync_pending_document(window, cx);
        let theme = cx.theme().colors;
        let body = match self.active_surface {
            None => self.render_chooser(cx).into_any_element(),
            Some(Surface::Review) => self.render_review(cx),
            Some(Surface::Files) => self.render_files(cx),
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

fn scan_project_files(
    root: &Path,
    expanded_paths: &HashSet<String>,
    limit: usize,
) -> Vec<FileEntry> {
    fn visit(
        root: &Path,
        relative: &Path,
        depth: usize,
        expanded_paths: &HashSet<String>,
        limit: usize,
        rows: &mut Vec<FileEntry>,
    ) {
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
            if is_dir && expanded_paths.contains(path.to_string_lossy().as_ref()) {
                visit(root, &path, depth + 1, expanded_paths, limit, rows);
            }
        }
    }

    let mut rows = Vec::new();
    visit(root, Path::new(""), 0, expanded_paths, limit, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::scan_project_files;
    use std::collections::HashSet;
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

        let collapsed = scan_project_files(&root, &HashSet::new(), 10);
        assert_eq!(
            collapsed
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["src"]
        );

        let expanded = HashSet::from(["src".to_string()]);
        let rows = scan_project_files(&root, &expanded, 10);
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
