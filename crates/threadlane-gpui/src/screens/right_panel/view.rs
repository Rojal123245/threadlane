use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState, TabSize};
use gpui_component::list::ListItem;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::separator::Separator;
use gpui_component::spinner::Spinner;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::tree::{Tree, TreeEvent, TreeItem, TreeState};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt};
use threadlane_git::{GitFile, GitStatus};

use crate::services::watcher::WorkspaceWatcher;
use crate::state::AppState;

fn normalize_generated_commit_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed)
        .trim();
    let without_fences = if unquoted.starts_with("```") {
        unquoted
            .lines()
            .filter(|line| !line.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        unquoted.to_string()
    };
    without_fences
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitAction {
    Commit,
    CommitAndPush,
    Push,
}

#[derive(Clone, Debug)]
struct FileNode {
    relative_path: String,
    name: String,
    is_dir: bool,
    children: Vec<FileNode>,
}

enum PanelEvent {
    FilesLoaded {
        project: PathBuf,
        nodes: Vec<FileNode>,
    },
    ReviewLoaded {
        project: PathBuf,
        status: Option<GitStatus>,
        files: Vec<GitFile>,
        error: Option<String>,
    },
    WorkspaceChanged {
        project: PathBuf,
        git_dirty: bool,
        files_dirty: bool,
    },
    MessageGenerated(Result<String, String>),
    ActionFinished(Result<GitStatus, String>),
}

pub struct RightPanelView {
    model: Entity<AppState>,
    active_surface: Option<Surface>,
    project: Option<PathBuf>,
    tree_state: Entity<TreeState>,
    expanded_paths: HashSet<String>,
    review_files: Vec<GitFile>,
    selected_files: HashSet<String>,
    git_status: Option<GitStatus>,
    review_error: Option<String>,
    commit_message_input: Entity<InputState>,
    generated_commit_message: Option<String>,
    should_clear_commit_message: bool,
    git_busy: bool,
    git_message_pending: bool,
    git_feedback: Option<String>,
    document_title: Option<String>,
    document_state: Entity<TextViewState>,
    editor_state: Option<Entity<EditorState>>,
    editor_subscription: Option<Subscription>,
    saved_content: String,
    is_dirty: bool,
    pending_document: Option<(String, String)>,
    event_tx: mpsc::Sender<PanelEvent>,
    _watcher: Option<WorkspaceWatcher>,
    _subscriptions: Vec<Subscription>,
}

impl RightPanelView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_state = cx.new(|cx| TextViewState::markdown("", cx));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let commit_message_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Summary (required)"));
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

        let observe_model = cx.observe(&model, |this, _model, cx| {
            this.sync_project(cx);
            cx.notify();
        });
        let tree_subscription = cx.subscribe(&tree_state, |this, _tree, event: &TreeEvent, _cx| {
            match event {
                TreeEvent::Expanded(id) => {
                    this.expanded_paths.insert(id.to_string());
                }
                TreeEvent::Collapsed(id) => {
                    this.expanded_paths.remove(id.as_ref());
                }
            }
        });

        let mut panel = Self {
            model,
            active_surface: None,
            project: None,
            tree_state,
            expanded_paths: HashSet::new(),
            review_files: Vec::new(),
            selected_files: HashSet::new(),
            git_status: None,
            review_error: None,
            commit_message_input,
            generated_commit_message: None,
            should_clear_commit_message: false,
            git_busy: false,
            git_message_pending: false,
            git_feedback: None,
            document_title: None,
            document_state,
            editor_state: None,
            editor_subscription: None,
            saved_content: String::new(),
            is_dirty: false,
            pending_document: None,
            event_tx,
            _watcher: None,
            _subscriptions: vec![observe_model, tree_subscription],
        };
        panel.sync_project(cx);
        panel
    }

    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let project = self.model.read(cx).active_work_dir.clone();
        if self.project == project {
            return;
        }
        self.project = project.clone();
        self.tree_state.update(cx, |state, cx| state.set_items(Vec::new(), cx));
        self.expanded_paths.clear();
        self.review_files.clear();
        self.selected_files.clear();
        self.review_error = None;
        self.document_title = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));

        if let Some(work_dir) = project {
            let tx = self.event_tx.clone();
            let proj = work_dir.clone();
            self._watcher = WorkspaceWatcher::start(
                work_dir,
                Duration::from_millis(200),
                move |change| {
                    let _ = tx.send(PanelEvent::WorkspaceChanged {
                        project: proj.clone(),
                        git_dirty: change.git_dirty,
                        files_dirty: change.files_dirty,
                    });
                },
            )
            .ok();
        } else {
            self._watcher = None;
        }

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
        std::thread::spawn(move || match surface {
            Surface::Files => {
                let nodes = scan_project_tree(&project, 500);
                let _ = tx.send(PanelEvent::FilesLoaded { project, nodes });
            }
            Surface::Review => {
                let (status, files, error) = match threadlane_git::inspect(&project) {
                    Ok(status) => {
                        let files = status.files.clone();
                        (Some(status), files, None)
                    }
                    Err(error) => (None, Vec::new(), Some(error.to_string())),
                };
                let _ = tx.send(PanelEvent::ReviewLoaded {
                    project,
                    status,
                    files,
                    error,
                });
            }
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
                EditorState::new(window, cx)
                    .language(lang)
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
        let target_path = project.join(title);
        let content = editor.read(cx).value().to_string();
        if std::fs::write(&target_path, &content).is_ok() {
            self.saved_content = content;
            self.is_dirty = false;
            cx.notify();
        }
    }

    fn apply_event(&mut self, event: PanelEvent, cx: &mut Context<Self>) {
        match event {
            PanelEvent::WorkspaceChanged {
                project,
                git_dirty,
                files_dirty,
            } if self.project.as_ref() == Some(&project) => {
                if git_dirty {
                    self.refresh_surface(Surface::Review);
                }
                if files_dirty {
                    self.refresh_surface(Surface::Files);
                }
            }
            PanelEvent::FilesLoaded { project, nodes }
                if self.project.as_ref() == Some(&project) =>
            {
                let expanded_paths = &self.expanded_paths;
                let items = nodes
                    .into_iter()
                    .map(|node| convert_node_to_tree_item(node, expanded_paths))
                    .collect::<Vec<_>>();
                self.tree_state.update(cx, |state, cx| state.set_items(items, cx));
            }
            PanelEvent::ReviewLoaded {
                project,
                status,
                files,
                error,
            } if self.project.as_ref() == Some(&project) => {
                if let Some(status_ref) = &status {
                    self.model.update(cx, |state, cx| {
                        state.git_statuses.insert(project.clone(), status_ref.clone());
                        cx.notify();
                    });
                }
                self.git_status = status;
                let current_set: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
                if self.selected_files.is_empty() {
                    self.selected_files = current_set;
                } else {
                    let kept: HashSet<String> = self
                        .selected_files
                        .iter()
                        .filter(|p| current_set.contains(*p))
                        .cloned()
                        .collect();
                    if kept.is_empty() {
                        self.selected_files = current_set;
                    } else {
                        self.selected_files = kept;
                    }
                }
                self.review_files = files;
                self.review_error = error;
            }
            PanelEvent::MessageGenerated(result) => {
                self.git_message_pending = false;
                match result {
                    Ok(message) => {
                        self.generated_commit_message = Some(message);
                        self.git_feedback = None;
                    }
                    Err(error) => {
                        self.git_feedback = Some(error);
                    }
                }
            }
            PanelEvent::ActionFinished(result) => {
                self.git_busy = false;
                match result {
                    Ok(status) => {
                        if let Some(project) = &self.project {
                            self.model.update(cx, |state, cx| {
                                state.git_statuses.insert(project.clone(), status.clone());
                                cx.notify();
                            });
                        }
                        self.git_status = Some(status.clone());
                        self.selected_files = status.files.iter().map(|f| f.path.clone()).collect();
                        self.review_files = status.files;
                        self.review_error = None;
                        self.should_clear_commit_message = true;
                        self.git_feedback = Some("Git action completed successfully.".into());
                    }
                    Err(error) => {
                        self.git_feedback = Some(error);
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn refresh_review(&mut self, _cx: &mut Context<Self>) {
        self.refresh_surface(Surface::Review);
    }

    fn generate_commit_message(&mut self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.project.clone() else {
            self.git_feedback = Some("Attach a project to generate a commit message.".into());
            cx.notify();
            return;
        };
        let selected_paths: Vec<String> = self.selected_files.iter().cloned().collect();
        if selected_paths.is_empty() {
            self.git_feedback =
                Some("Select at least one file to generate a commit message.".into());
            cx.notify();
            return;
        }
        let total_count = self.review_files.len();
        let model = self.model.read(cx).selected_model.clone();
        if model.is_empty() {
            self.git_feedback =
                Some("Select a model in chat before generating a commit message.".into());
            cx.notify();
            return;
        }
        let (api_key, account_id) = crate::state::provider_credentials(&model);
        let tx = self.event_tx.clone();
        let Ok(executor) = crate::services::chat::executor() else {
            self.git_feedback = Some("Unable to start the model runtime.".into());
            cx.notify();
            return;
        };

        self.git_message_pending = true;
        self.git_feedback = Some("Generating a commit message…".into());
        executor.spawn(async move {
            let result = async {
                let diff = if selected_paths.len() == total_count {
                    threadlane_git::commit_message_diff(&work_dir)
                        .map_err(|error| error.to_string())?
                } else {
                    let mut diffs = Vec::new();
                    for path in &selected_paths {
                        if let Ok(d) = threadlane_git::diff_file(&work_dir, path) {
                            if !d.trim().is_empty() {
                                diffs.push(d);
                            }
                        }
                    }
                    diffs.join("\n")
                };
                let diff = if diff.chars().count() > 24_000 {
                    format!(
                        "{}\n\n[Diff truncated for message generation]",
                        diff.chars().take(24_000).collect::<String>()
                    )
                } else {
                    diff
                };
                let raw = threadlane_provider::ProviderClient::new(api_key, account_id)
                    .generate_commit_message(&model, &diff)
                    .await?;
                let message = normalize_generated_commit_message(&raw);
                if message.is_empty() {
                    Err("The model returned an empty commit message.".to_string())
                } else {
                    Ok(message)
                }
            }
            .await;
            let _ = tx.send(PanelEvent::MessageGenerated(result));
        });
        cx.notify();
    }

    fn run_git_action(
        &mut self,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(work_dir) = self.project.clone() else {
            self.git_feedback = Some("Attach a project to use Git actions.".into());
            window.push_notification(
                Notification::warning("Attach a project to use Git actions"),
                cx,
            );
            cx.notify();
            return;
        };
        let message = self.commit_message_input.read(cx).value().trim().to_string();
        let selected_paths: Vec<String> = self.selected_files.iter().cloned().collect();

        if matches!(action, GitAction::Commit | GitAction::CommitAndPush) {
            if selected_paths.is_empty() {
                self.git_feedback = Some("Select at least one file to commit.".into());
                window.push_notification(
                    Notification::warning("Select at least one file to commit"),
                    cx,
                );
                cx.notify();
                return;
            }
            if message.is_empty() {
                self.git_feedback = Some("Enter a commit message first.".into());
                window.push_notification(Notification::warning("Enter a commit message first"), cx);
                cx.notify();
                return;
            }
        }

        self.git_busy = true;
        let feedback = match action {
            GitAction::Commit => "Committing…",
            GitAction::CommitAndPush => "Committing and pushing…",
            GitAction::Push => "Pushing…",
        };
        self.git_feedback = Some(feedback.into());
        window.push_notification(Notification::info(feedback), cx);
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let status = threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())?;
                if matches!(action, GitAction::Commit | GitAction::CommitAndPush) {
                    let selected_set: HashSet<&str> =
                        selected_paths.iter().map(String::as_str).collect();
                    for file in &status.files {
                        if selected_set.contains(file.path.as_str()) {
                            threadlane_git::stage_file(&work_dir, &file.path)
                                .map_err(|e| e.to_string())?;
                        } else {
                            let _ = threadlane_git::unstage_file(&work_dir, &file.path);
                        }
                    }
                    threadlane_git::commit_staged(&work_dir, &message)
                        .map_err(|e| e.to_string())?;
                }
                if matches!(action, GitAction::CommitAndPush | GitAction::Push) {
                    threadlane_git::push(&work_dir).map_err(|e| e.to_string())?;
                }
                threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())
            })();
            let _ = tx.send(PanelEvent::ActionFinished(result));
        });
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let active = self.active_surface;
        div()
            .flex_none()
            .pt(px(44.0))
            .pb_2()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children([Surface::Review, Surface::Files].map(|surface| {
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
                    }))
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
                    ),
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
                                .children(
                                    is_dirty.then(|| Tag::warning().child("modified").xsmall()),
                                )
                                .children(
                                    has_editor
                                        .then(|| Tag::secondary().child(lang).outline().xsmall()),
                                ),
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
        let model = self.model.clone();

        div()
            .flex_1()
            .min_h_0()
            .py_2()
            .child(
                Tree::new(&self.tree_state, move |ix, entry, is_selected, _window, cx| {
                    let relative_path = entry.item().id.to_string();
                    let name = entry.item().label.to_string();
                    let is_folder = entry.is_folder();
                    let is_expanded = entry.is_expanded();
                    let depth = entry.depth();

                    let target_path = relative_path.clone();
                    let click_model = model.clone();
                    let theme = cx.theme().colors;

                    ListItem::new(format!("tree-item-{ix}"))
                        .mx_1()
                        .rounded_md()
                        .px_1p5()
                        .py_1()
                        .pl(px(6.0 + depth as f32 * 12.0))
                        .selected(is_selected)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_xs()
                                .text_color(if is_selected {
                                    theme.foreground
                                } else {
                                    theme.muted_foreground
                                })
                                .child(if is_folder {
                                    div()
                                        .w(px(14.0))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(if is_expanded {
                                            Icon::new(IconName::ChevronDown)
                                                .xsmall()
                                                .into_any_element()
                                        } else {
                                            Icon::new(IconName::ChevronRight)
                                                .xsmall()
                                                .into_any_element()
                                        })
                                        .into_any_element()
                                } else {
                                    div().w(px(14.0)).flex_none().into_any_element()
                                })
                                .child(if is_folder {
                                    Icon::new(IconName::Folder).xsmall().into_any_element()
                                } else {
                                    Icon::new(IconName::File).xsmall().into_any_element()
                                })
                                .child(name),
                        )
                        .when(!is_folder, move |item| {
                            item.on_click(move |_event, _window, cx| {
                                click_model.update(cx, |state, cx| {
                                    state.request_open_file(target_path.clone());
                                    cx.notify();
                                });
                            })
                        })
                })
                .context_menu({
                    let model = self.model.clone();
                    let project = self.project.clone();
                    move |_ix, entry, menu, _window, _cx| {
                        let relative_path = entry.item().id.to_string();
                        let is_folder = entry.is_folder();
                        let absolute_path = project
                            .as_ref()
                            .map(|p| p.join(&relative_path).display().to_string());
                        let ed_path = relative_path.clone();
                        let text = relative_path.clone();
                        let model_ref = model.clone();

                        let mut menu = menu;
                        if !is_folder {
                            menu = menu.item(PopupMenuItem::new("Open in Editor Tab").on_click(
                                move |_event, _window, cx| {
                                    model_ref.update(cx, |state, cx| {
                                        state.request_open_file(ed_path.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu = menu.item(PopupMenuItem::new("Copy Relative Path").on_click(
                            move |_event, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                window.push_notification(
                                    Notification::info("Copied relative path"),
                                    cx,
                                );
                            },
                        ));
                        if let Some(abs) = absolute_path {
                            menu = menu.item(PopupMenuItem::new("Copy Absolute Path").on_click(
                                move |_event, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(abs.clone()));
                                    window.push_notification(
                                        Notification::info("Copied absolute path"),
                                        cx,
                                    );
                                },
                            ));
                        }
                        menu
                    }
                }),
            )
            .into_any_element()
    }

    fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if let Some(error) = &self.review_error {
            return self.render_empty("Review unavailable", error, cx);
        }
        let total_files = self.review_files.len();
        let selected_count = self.selected_files.len();
        let all_selected = total_files > 0 && selected_count == total_files;

        let selected_additions: u32 = self
            .review_files
            .iter()
            .filter(|f| self.selected_files.contains(&f.path))
            .map(|f| f.additions)
            .sum();
        let selected_deletions: u32 = self
            .review_files
            .iter()
            .filter(|f| self.selected_files.contains(&f.path))
            .map(|f| f.deletions)
            .sum();

        let branch = self
            .git_status
            .as_ref()
            .and_then(|s| s.branch.as_deref())
            .unwrap_or("No branch");

        let can_commit = !self.git_busy && !self.git_message_pending && selected_count > 0;
        let can_push = !self.git_busy
            && !self.git_message_pending
            && self
                .git_status
                .as_ref()
                .is_some_and(|status| status.ahead > 0);

        let branch_header = div()
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
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/git/branch.svg")),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(branch.to_string()),
                    ),
            )
            .child(
                Button::new("refresh-review-btn")
                    .icon(IconName::Redo)
                    .tooltip("Refresh Git Status")
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.refresh_review(cx);
                    })),
            );

        let pr_card = self.git_status.as_ref().and_then(|s| s.pr.as_ref()).map(|pr| {
            let pr_url = pr.url.clone();
            let pr_num = pr.number;
            let pr_title = pr.title.clone();
            let pr_title_display = if pr.title.is_empty() {
                format!("PR #{pr_num}")
            } else {
                format!("#{pr_num} {}", pr.title)
            };

            let failing_checks = pr.failing_checks;
            let pending_checks = pr.pending_checks;
            let total_checks = pr.total_checks;
            let comments_count = pr.comments_count;

            let failing_check_names: Vec<String> = pr
                .checks
                .iter()
                .filter(|c| {
                    let concl = c.conclusion.as_deref().unwrap_or("").to_uppercase();
                    matches!(
                        concl.as_str(),
                        "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
                    )
                })
                .map(|c| c.name.clone())
                .collect();
            let failed_summary = failing_check_names.join(", ");

            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .p_2p5()
                .mx_2()
                .my_1p5()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.2))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .size(px(16.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(theme.muted_foreground)
                                        .child(Icon::default().path("icons/git/actions.svg")),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(pr_title_display),
                                ),
                        )
                        .when(!pr_url.is_empty(), |row| {
                            let target_url = pr_url.clone();
                            row.child(
                                Button::new("pr-link-btn")
                                    .icon(IconName::ExternalLink)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Open pull request in browser")
                                    .on_click(move |_event, _window, cx| {
                                        cx.open_url(&target_url);
                                    }),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .pt_0p5()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .min_w_0()
                                .child(
                                    div()
                                        .size(px(14.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(if failing_checks > 0 {
                                            theme.danger
                                        } else if pending_checks > 0 {
                                            theme.warning
                                        } else {
                                            theme.success
                                        })
                                        .child(if failing_checks > 0 {
                                            IconName::Close
                                        } else if pending_checks > 0 {
                                            IconName::Asterisk
                                        } else {
                                            IconName::Check
                                        }),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(if failing_checks > 0 {
                                            theme.danger
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(if failing_checks > 0 {
                                            format!(
                                                "{failing_checks} failing check{}",
                                                if failing_checks == 1 { "" } else { "s" }
                                            )
                                        } else if pending_checks > 0 {
                                            format!("{pending_checks} in progress")
                                        } else {
                                            format!("All {} checks passed", total_checks.max(1))
                                        }),
                                ),
                        )
                        .child(if failing_checks > 0 {
                            let fix_pr_num = pr_num;
                            let fix_pr_title = pr_title.clone();
                            let fix_failed_summary = failed_summary.clone();
                            Button::new("fix-ci-btn")
                                .label("Fix CI")
                                .danger()
                                .xsmall()
                                .tooltip("Ask AI to fix failing CI checks")
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    let prompt = format!(
                                        "Please inspect and fix the failing CI check on PR #{fix_pr_num} ({fix_pr_title}): {fix_failed_summary}"
                                    );
                                    this.model.update(cx, |state, _cx| {
                                        state.request_composer_prompt(prompt);
                                    });
                                    cx.notify();
                                }))
                                .into_any_element()
                        } else {
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}/{}", pr.passing_checks, pr.total_checks))
                                .into_any_element()
                        }),
                )
                .when(comments_count > 0, |card| {
                    let comments_pr_num = pr_num;
                    let comments_pr_title = pr_title.clone();
                    card.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pt_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .size(px(14.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(theme.muted_foreground)
                                            .child(IconName::File),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "{comments_count} review comment{}",
                                                if comments_count == 1 { "" } else { "s" }
                                            )),
                                    ),
                            )
                            .child(
                                Button::new("address-comments-btn")
                                    .label("Address")
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Ask AI to address PR comments")
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        let prompt = format!(
                                            "Please review and address comments and feedback on PR #{comments_pr_num} ({comments_pr_title})."
                                        );
                                        this.model.update(cx, |state, _cx| {
                                            state.request_composer_prompt(prompt);
                                        });
                                        cx.notify();
                                    })),
                            ),
                    )
                })
        });

        let selection_bar = (total_files > 0).then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.15))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Checkbox::new("select-all-files")
                                .checked(all_selected)
                                .small()
                                .on_click(cx.listener(move |this, checked, _window, cx| {
                                    if *checked {
                                        this.selected_files =
                                            this.review_files.iter().map(|f| f.path.clone()).collect();
                                    } else {
                                        this.selected_files.clear();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(if all_selected {
                                    format!("{total_files} changed files")
                                } else {
                                    format!("{selected_count} of {total_files} selected")
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            Tag::new()
                                .child(format!("+{selected_additions}"))
                                .with_variant(TagVariant::Success)
                                .small(),
                        )
                        .child(
                            Tag::new()
                                .child(format!("−{selected_deletions}"))
                                .with_variant(TagVariant::Danger)
                                .small(),
                        ),
                )
        });

        let file_list_content = if self.review_files.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child("No changes"),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("The working tree is clean."),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .py_1()
                .children(self.review_files.iter().cloned().map(|file| {
                    let path = file.path.clone();
                    let path_for_chk = path.clone();
                    let is_selected = self.selected_files.contains(&path);
                    let absolute_path = self
                        .project
                        .as_ref()
                        .map(|root| root.join(&path).display().to_string());
                    let status = file.status_char().to_string();
                    let status_color = match file.status_char() {
                        'A' | '?' => theme.success,
                        'D' => theme.danger,
                        _ => theme.warning,
                    };
                    let context_path = path.clone();
                    div()
                        .id(SharedString::from(format!("review-file-{path}")))
                        .h(px(32.0))
                        .mx_2()
                        .px_2()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .hover(|row| row.bg(theme.muted))
                        .child(
                            Checkbox::new(SharedString::from(format!("chk-{path}")))
                                .checked(is_selected)
                                .small()
                                .on_click(cx.listener(move |this, checked, _window, cx| {
                                    if *checked {
                                        this.selected_files.insert(path_for_chk.clone());
                                    } else {
                                        this.selected_files.remove(&path_for_chk);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("review-file-btn-{path}")))
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
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
                                        .w(px(16.0))
                                        .text_center()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(status_color)
                                        .child(status),
                                )
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    let target_path = path.clone();
                                    let Some(project) = this.project.clone() else {
                                        return;
                                    };
                                    let diff_project = project.clone();
                                    let model = this.model.clone();
                                    cx.spawn(async move |_this, cx| {
                                        let diff_target = target_path.clone();
                                        let content = cx
                                            .background_executor()
                                            .spawn(async move {
                                                threadlane_git::diff_file(&diff_project, &diff_target)
                                                    .unwrap_or_else(|error| error.to_string())
                                            })
                                            .await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.request_open_diff(project, target_path, content);
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                })),
                        )
                        .context_menu({
                            let path = context_path.clone();
                            let absolute_path = absolute_path.clone();
                            let project = self.project.clone();
                            let model = self.model.clone();
                            move |menu, _window, _cx| {
                                let diff_path = path.clone();
                                let project_ref = project.clone();
                                let model_ref = model.clone();
                                let mut menu = menu.item(
                                    PopupMenuItem::new("Open Diff in Editor Tab").on_click(
                                        move |_event, _window, cx| {
                                            let Some(proj) = project_ref.clone() else {
                                                return;
                                            };
                                            let diff_project = proj.clone();
                                            let target = diff_path.clone();
                                            let m = model_ref.clone();
                                            cx.spawn(async move |cx| {
                                                let diff_target = target.clone();
                                                let content = cx
                                                    .background_executor()
                                                    .spawn(async move {
                                                        threadlane_git::diff_file(
                                                            &diff_project,
                                                            &diff_target,
                                                        )
                                                        .unwrap_or_else(|error| {
                                                            error.to_string()
                                                        })
                                                    })
                                                    .await;
                                                let _ = m.update(cx, |state, cx| {
                                                    state.request_open_diff(
                                                        proj, target, content,
                                                    );
                                                    cx.notify();
                                                });
                                            })
                                            .detach();
                                        },
                                    ),
                                );
                                if let Some(absolute_path) = absolute_path.clone() {
                                    let text = absolute_path;
                                    menu = menu.separator().item(
                                        PopupMenuItem::new("Copy Absolute Path").on_click(
                                            move |_event, window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text.clone(),
                                                ));
                                                window.push_notification(
                                                    Notification::info("Copied absolute path"),
                                                    cx,
                                                );
                                            },
                                        ),
                                    );
                                }
                                menu
                            }
                        })
                }))
                .into_any_element()
        };

        let commit_label = if selected_count > 0 && selected_count < total_files {
            format!("Commit {selected_count}")
        } else {
            "Commit".to_string()
        };
        let commit_push_label = if selected_count > 0 && selected_count < total_files {
            format!("Commit {selected_count} & push")
        } else {
            "Commit & push".to_string()
        };

        let commit_footer = div()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("COMMIT"),
                    )
                    .child(
                        Button::new("git-generate-message")
                            .when(self.git_message_pending, |button| {
                                button
                                    .child(Spinner::new().xsmall())
                                    .label("Generating…")
                            })
                            .when(!self.git_message_pending, |button| {
                                button.label("Generate")
                            })
                            .ghost()
                            .xsmall()
                            .disabled(
                                self.git_busy
                                    || self.git_message_pending
                                    || selected_count == 0,
                            )
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.generate_commit_message(cx);
                            })),
                    ),
            )
            .child(
                Input::new(&self.commit_message_input)
                    .disabled(self.git_busy),
            )
            .children(self.git_feedback.as_ref().map(|feedback| {
                div()
                    .rounded_md()
                    .bg(theme.muted)
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(feedback.clone())
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("git-commit-push")
                            .icon(Icon::default().path("icons/git/commit.svg"))
                            .label(commit_push_label)
                            .primary()
                            .small()
                            .flex_1()
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.run_git_action(GitAction::CommitAndPush, window, cx);
                            })),
                    )
                    .child(
                        Button::new("git-commit-only")
                            .label(commit_label)
                            .outline()
                            .small()
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.run_git_action(GitAction::Commit, window, cx);
                            })),
                    )
                    .when(can_push, |row| {
                        row.child(
                            Button::new("git-push-only")
                                .icon(Icon::default().path("icons/git/actions.svg"))
                                .tooltip("Push commits")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.run_git_action(GitAction::Push, window, cx);
                                })),
                        )
                    }),
            );

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(branch_header)
            .children(pr_card)
            .children(selection_bar)
            .child(file_list_content)
            .child(commit_footer)
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
        if let Some(message) = self.generated_commit_message.take() {
            self.commit_message_input
                .update(cx, |input, cx| input.set_value(message, window, cx));
        }
        if self.should_clear_commit_message {
            self.should_clear_commit_message = false;
            self.commit_message_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
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
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(body)
    }
}

fn convert_node_to_tree_item(node: FileNode, expanded_paths: &HashSet<String>) -> TreeItem {
    let is_expanded = expanded_paths.contains(&node.relative_path);
    if node.is_dir {
        let children = node
            .children
            .into_iter()
            .map(|child| convert_node_to_tree_item(child, expanded_paths))
            .collect::<Vec<_>>();
        TreeItem::new(node.relative_path, node.name)
            .expanded(is_expanded)
            .children(children)
    } else {
        TreeItem::new(node.relative_path, node.name)
    }
}

fn scan_project_tree(
    root: &Path,
    limit: usize,
) -> Vec<FileNode> {
    fn visit(
        root: &Path,
        relative: &Path,
        depth: usize,
        limit: usize,
        count: &mut usize,
    ) -> Vec<FileNode> {
        if *count >= limit || depth > 6 {
            return Vec::new();
        }
        let Ok(read_dir) = std::fs::read_dir(root.join(relative)) else {
            return Vec::new();
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

        let mut nodes = Vec::new();
        for (name, is_dir) in children {
            if *count >= limit {
                break;
            }
            *count += 1;
            let path = relative.join(&name);
            let rel_str = path.to_string_lossy().into_owned();
            if is_dir {
                let sub_children = visit(root, &path, depth + 1, limit, count);
                nodes.push(FileNode {
                    relative_path: rel_str,
                    name,
                    is_dir: true,
                    children: sub_children,
                });
            } else {
                nodes.push(FileNode {
                    relative_path: rel_str,
                    name,
                    is_dir: false,
                    children: Vec::new(),
                });
            }
        }
        nodes
    }

    let mut count = 0;
    visit(root, Path::new(""), 0, limit, &mut count)
}

#[cfg(test)]
mod tests {
    use super::scan_project_tree;
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

        let items = scan_project_tree(&root, 10);
        assert_eq!(
            items
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["src"]
        );
        assert!(items[0].children.iter().any(|item| item.relative_path == "src/main.rs"));
        assert!(items[0].children.iter().any(|item| item.relative_path == "src/nested"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
