use std::sync::mpsc;
use std::time::Duration;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};

use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use threadlane_git::GitStatus;

use crate::screens::chat::ChatListView;
use crate::screens::right_panel::RightPanelView;
use crate::screens::settings::SettingsView;
use crate::screens::sidebar::SidebarView;
use crate::services::updater::{self, UpdaterEvent};
use crate::state::{AppState, WorkspacePage};
use threadlane_updater::UpdateStatus;

enum GitAction {
    Commit,
    CommitAndPush,
    Push,
}

enum GitEvent {
    Loaded(Result<GitStatus, String>),
    Finished(Result<GitStatus, String>),
    MessageGenerated(Result<String, String>),
}

fn normalize_generated_commit_message(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))
        .unwrap_or_default()
        .trim_matches('`')
        .trim();
    let line = line
        .strip_prefix("Commit message:")
        .or_else(|| line.strip_prefix("Commit:"))
        .unwrap_or(line)
        .trim();
    line.chars().take(72).collect()
}

pub struct WorkspaceView {
    model: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
    settings: Entity<SettingsView>,
    right_panel: Entity<RightPanelView>,
    sidebar_collapsed: bool,
    right_panel_visible: bool,
    environment_open: bool,
    git_dialog_open: bool,
    git_include_unstaged: bool,
    git_busy: bool,
    git_message_pending: bool,
    generated_commit_message: Option<String>,
    git_status: Option<GitStatus>,
    git_feedback: Option<String>,
    git_message_input: Entity<InputState>,
    git_event_tx: mpsc::Sender<GitEvent>,
    updater_tx: mpsc::Sender<UpdaterEvent>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| AppState::load());
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model.clone(), window, cx));
        let settings = cx.new(|cx| SettingsView::new(model.clone(), window, cx));
        let right_panel = cx.new(|cx| RightPanelView::new(model.clone(), window, cx));
        let git_message_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let (git_event_tx, git_event_rx) = mpsc::channel();
        let (updater_tx, updater_rx) = mpsc::channel();

        #[cfg(target_os = "macos")]
        if threadlane_updater::is_configured() {
            updater::check(updater_tx.clone());
        }

        let model_clone = model.clone();
        cx.new(|cx| {
            let sub = cx.observe(&model_clone, |_this: &mut Self, _model, cx| {
                cx.notify();
            });

            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let git_events = git_event_rx.try_iter().collect::<Vec<_>>();
                let updater_events = updater_rx.try_iter().collect::<Vec<_>>();
                if git_events.is_empty() && updater_events.is_empty() {
                    continue;
                }
                let _ = this.update(cx, |this, cx| {
                    for event in git_events {
                        this.apply_git_event(event, cx);
                    }
                    for UpdaterEvent::Status(status) in updater_events {
                        this.model.update(cx, |state, cx| {
                            state.update_status = status;
                            state.update_notice_dismissed = false;
                            cx.notify();
                        });
                    }
                    cx.notify();
                });
            })
            .detach();

            Self {
                model,
                sidebar,
                chat_list,
                settings,
                right_panel,
                sidebar_collapsed: false,
                right_panel_visible: false,
                environment_open: false,
                git_dialog_open: false,
                git_include_unstaged: true,
                git_busy: false,
                git_message_pending: false,
                generated_commit_message: None,
                git_status: None,
                git_feedback: None,
                git_message_input,
                git_event_tx,
                updater_tx,
                _subscriptions: vec![sub],
            }
        })
    }

    fn open_git_dialog(&mut self, cx: &mut Context<Self>) {
        self.git_dialog_open = true;
        self.git_feedback = None;
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn refresh_git_status(&mut self, cx: &App) {
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            self.git_status = None;
            self.git_feedback = Some("Attach a project to use Git actions.".into());
            return;
        };
        self.git_busy = true;
        let tx = self.git_event_tx.clone();
        std::thread::spawn(move || {
            let result = threadlane_git::inspect(&work_dir).map_err(|error| error.to_string());
            let _ = tx.send(GitEvent::Loaded(result));
        });
    }

    fn generate_commit_message(&mut self, cx: &mut Context<Self>) {
        if self.git_busy || self.git_message_pending {
            return;
        }
        if !self.git_message_input.read(cx).value().trim().is_empty() {
            self.git_feedback =
                Some("Clear the current message before generating a new one.".into());
            cx.notify();
            return;
        }
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            self.git_feedback = Some("Attach a project to generate a commit message.".into());
            cx.notify();
            return;
        };
        let model = self.model.read(cx).selected_model.clone();
        if model.trim().is_empty() {
            self.git_feedback = Some("Select a model before generating a commit message.".into());
            cx.notify();
            return;
        }
        let (api_key, account_id) = crate::state::provider_credentials(&model);
        let tx = self.git_event_tx.clone();
        let Ok(executor) = crate::services::chat::executor() else {
            self.git_feedback = Some("Unable to start the model runtime.".into());
            cx.notify();
            return;
        };

        self.git_message_pending = true;
        self.git_feedback = Some("Generating a commit message…".into());
        executor.spawn(async move {
            let result = async {
                let diff = threadlane_git::commit_message_diff(&work_dir)
                    .map_err(|error| error.to_string())?;
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
            let _ = tx.send(GitEvent::MessageGenerated(result));
        });
        cx.notify();
    }

    fn run_git_action(&mut self, action: GitAction, cx: &mut Context<Self>) {
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            self.git_feedback = Some("Attach a project to use Git actions.".into());
            cx.notify();
            return;
        };
        let message = self.git_message_input.read(cx).value().trim().to_string();
        if !matches!(action, GitAction::Push) && message.is_empty() {
            self.git_feedback = Some("Enter a commit message first.".into());
            cx.notify();
            return;
        }

        self.git_busy = true;
        self.git_feedback = Some(
            match action {
                GitAction::Commit => "Committing…",
                GitAction::CommitAndPush => "Committing and pushing…",
                GitAction::Push => "Pushing…",
            }
            .into(),
        );
        let include_unstaged = self.git_include_unstaged;
        let tx = self.git_event_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                if !matches!(action, GitAction::Push) && include_unstaged {
                    let status = threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())?;
                    for file in status.files.iter().filter(|file| file.unstaged) {
                        threadlane_git::stage_file(&work_dir, &file.path)
                            .map_err(|e| e.to_string())?;
                    }
                }
                if !matches!(action, GitAction::Push) {
                    threadlane_git::commit_staged(&work_dir, &message)
                        .map_err(|e| e.to_string())?;
                }
                if matches!(action, GitAction::CommitAndPush | GitAction::Push) {
                    threadlane_git::push(&work_dir).map_err(|e| e.to_string())?;
                }
                threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())
            })();
            let _ = tx.send(GitEvent::Finished(result));
        });
        cx.notify();
    }

    fn apply_git_event(&mut self, event: GitEvent, cx: &mut Context<Self>) {
        self.git_busy = false;
        match event {
            GitEvent::Loaded(Ok(status)) => {
                self.git_status = Some(status);
            }
            GitEvent::Loaded(Err(error)) => self.git_feedback = Some(error),
            GitEvent::Finished(Ok(status)) => {
                self.git_status = Some(status);
                self.git_feedback = Some("Git action completed.".into());
                self.right_panel
                    .update(cx, |panel, cx| panel.open_review(cx));
            }
            GitEvent::Finished(Err(error)) => self.git_feedback = Some(error),
            GitEvent::MessageGenerated(Ok(message)) => {
                self.git_message_pending = false;
                self.generated_commit_message = Some(message);
                self.git_feedback = None;
            }
            GitEvent::MessageGenerated(Err(error)) => {
                self.git_message_pending = false;
                self.git_feedback = Some(error);
            }
        }
    }

    fn render_environment_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;

        div()
            .absolute()
            .top(px(48.0))
            .right(px(44.0))
            .w(px(276.0))
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("Environment"),
            )
            .child(
                div()
                    .id("environment-commit")
                    .h(px(44.0))
                    .w_full()
                    .px_3()
                    .rounded_lg()
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.list_hover))
                    .child(
                        div()
                            .size(px(18.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/git/commit.svg")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Commit or push"),
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.environment_open = false;
                        this.open_git_dialog(cx);
                    })),
            )
            .child(
                div()
                    .id("environment-compare")
                    .h(px(44.0))
                    .w_full()
                    .px_3()
                    .rounded_lg()
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.list_hover))
                    .child(
                        div()
                            .size(px(18.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/git/compare.svg")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Compare branch"),
                    )
                    .child(
                        div()
                            .size(px(16.0))
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::ExternalLink)),
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.environment_open = false;
                        this.right_panel_visible = true;
                        this.right_panel
                            .update(cx, |panel, cx| panel.open_review(cx));
                        cx.notify();
                    })),
            )
    }

    fn render_git_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let status = self.git_status.as_ref();
        let branch = status
            .and_then(|status| status.branch.as_deref())
            .unwrap_or("No branch");
        let additions = status
            .map(|status| status.files.iter().map(|file| file.additions).sum::<u32>())
            .unwrap_or(0);
        let deletions = status
            .map(|status| status.files.iter().map(|file| file.deletions).sum::<u32>())
            .unwrap_or(0);
        let can_commit = !self.git_busy
            && !self.git_message_pending
            && status.is_some_and(|status| {
                status.staged_changes || (self.git_include_unstaged && status.unstaged_changes)
            });
        let can_push = !self.git_busy
            && !self.git_message_pending
            && status.is_some_and(|status| status.ahead > 0);
        let toggle_view = cx.entity().downgrade();

        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000099))
            .child(
                div()
                    .w(px(440.0))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_5()
                            .pt_5()
                            .pb_4()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(38.0))
                                    .rounded_lg()
                                    .bg(theme.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme.foreground)
                                    .child(Icon::default().path("icons/git/actions.svg")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Commit changes"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("On branch {branch}")),
                                    ),
                            )
                            .child(
                                Button::new("git-dialog-close")
                                    .icon(IconName::Close)
                                    .tooltip("Close")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        if !this.git_busy {
                                            this.git_dialog_open = false;
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .mx_5()
                            .mb_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .px_3()
                            .h(px(46.0))
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(Icon::default().path("icons/git/compare.svg"))
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(branch.to_string()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(theme.success)
                                            .child(format!("+{additions}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.danger)
                                            .child(format!("−{deletions}")),
                                    )
                                    .child(div().text_color(theme.muted_foreground).child(
                                        format!("{} files", status.map_or(0, |s| s.files.len())),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .px_5()
                            .pb_5()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Commit message"),
                                    )
                                    .child(
                                        Button::new("git-generate-message")
                                            .label(if self.git_message_pending {
                                                "Generating…"
                                            } else {
                                                "Generate"
                                            })
                                            .ghost()
                                            .xsmall()
                                            .disabled(self.git_busy || self.git_message_pending)
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.generate_commit_message(cx);
                                            })),
                                    ),
                            )
                            .child(Input::new(&self.git_message_input))
                            .child(
                                div()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme.border)
                                    .px_3()
                                    .py_3()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("Include unstaged changes"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child("Stage modified and untracked files"),
                                            ),
                                    )
                                    .child(
                                        Switch::new("git-include-unstaged")
                                            .checked(self.git_include_unstaged)
                                            .disabled(self.git_busy)
                                            .on_click(move |checked, _window, cx| {
                                                let checked = *checked;
                                                let _ = toggle_view.update(cx, |this, cx| {
                                                    this.git_include_unstaged = checked;
                                                    cx.notify();
                                                });
                                            }),
                                    ),
                            )
                            .children(self.git_feedback.as_ref().map(|feedback| {
                                div()
                                    .rounded_md()
                                    .bg(theme.muted)
                                    .px_3()
                                    .py_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(feedback.clone())
                            })),
                    )
                    .child(
                        div()
                            .border_t_1()
                            .border_color(theme.border)
                            .bg(theme.title_bar)
                            .px_5()
                            .py_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                Button::new("git-push")
                                    .label("Push only")
                                    .ghost()
                                    .disabled(!can_push)
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.run_git_action(GitAction::Push, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("git-commit")
                                            .label("Commit")
                                            .outline()
                                            .disabled(!can_commit)
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.run_git_action(GitAction::Commit, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("git-commit-push")
                                            .icon(Icon::default().path("icons/git/commit.svg"))
                                            .label("Commit & push")
                                            .disabled(!can_commit)
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.run_git_action(GitAction::CommitAndPush, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
    }

    fn render_update_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = {
            let state = self.model.read(cx);
            if state.update_notice_dismissed {
                return None;
            }
            state.update_status.clone()
        };

        let (title, detail) = match &status {
            UpdateStatus::Available(info) => (
                format!("Threadlane {} is available", info.version),
                "Download the verified update in the background.".to_string(),
            ),
            UpdateStatus::Downloading { progress, .. } => (
                "Downloading update".to_string(),
                format!("{}% complete", (progress.clamp(0.0, 1.0) * 100.0).round()),
            ),
            UpdateStatus::ReadyToInstall { info, .. } => (
                format!("Threadlane {} is ready", info.version),
                "Install the update and relaunch Threadlane.".to_string(),
            ),
            UpdateStatus::Installing => (
                "Installing update".to_string(),
                "Threadlane will relaunch when installation finishes.".to_string(),
            ),
            UpdateStatus::Error(error) => (
                "Update failed".to_string(),
                error.chars().take(160).collect(),
            ),
            _ => return None,
        };

        let action = match &status {
            UpdateStatus::Available(info) => {
                let tx = self.updater_tx.clone();
                let info = info.clone();
                Some(
                    Button::new("update-download")
                        .label("Download")
                        .primary()
                        .on_click(move |_event, _window, _cx| {
                            updater::download(info.clone(), tx.clone());
                        }),
                )
            }
            UpdateStatus::ReadyToInstall { info, bytes } => {
                let tx = self.updater_tx.clone();
                let info = info.clone();
                let bytes = bytes.clone();
                Some(
                    Button::new("update-install")
                        .label("Install and relaunch")
                        .primary()
                        .on_click(move |_event, _window, _cx| {
                            updater::install(info.clone(), bytes.clone(), tx.clone());
                        }),
                )
            }
            UpdateStatus::Error(_) => {
                let tx = self.updater_tx.clone();
                Some(
                    Button::new("update-retry")
                        .label("Retry")
                        .outline()
                        .on_click(move |_event, _window, _cx| updater::check(tx.clone())),
                )
            }
            _ => None,
        };
        let theme = cx.theme().colors;
        let model = self.model.clone();

        Some(
            div()
                .absolute()
                .right(px(16.0))
                .bottom(px(16.0))
                .w(px(420.0))
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .p_4()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(title),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        ),
                )
                .children(action)
                .children(
                    matches!(status, UpdateStatus::Available(_) | UpdateStatus::Error(_)).then(
                        || {
                            Button::new("update-dismiss")
                                .icon(IconName::Close)
                                .tooltip("Dismiss")
                                .ghost()
                                .xsmall()
                                .on_click(move |_event, _window, cx| {
                                    model.update(cx, |state, cx| {
                                        state.update_notice_dismissed = true;
                                        cx.notify();
                                    });
                                })
                        },
                    ),
                )
                .into_any_element(),
        )
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(message) = self.generated_commit_message.take() {
            self.git_message_input
                .update(cx, |input, cx| input.set_value(message, window, cx));
        }
        let workspace_page = self.model.read(cx).workspace_page;
        let theme = cx.theme().colors;
        let sidebar_tooltip = if self.sidebar_collapsed {
            "Show sidebar"
        } else {
            "Collapse sidebar"
        };

        div()
            .relative()
            .flex()
            .w_full()
            .h_full()
            .bg(theme.background)
            .children(
                (workspace_page == WorkspacePage::Chat && !self.sidebar_collapsed)
                    .then(|| self.sidebar.clone()),
            )
            .child(match workspace_page {
                WorkspacePage::Chat => div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(360.0))
                            .h_full()
                            .child(self.chat_list.clone()),
                    )
                    .children(self.right_panel_visible.then(|| {
                        div()
                            .flex_1()
                            .min_w(px(360.0))
                            .h_full()
                            .child(self.right_panel.clone())
                    }))
                    .into_any_element(),
                WorkspacePage::Settings => self.settings.clone().into_any_element(),
            })
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("environment-menu")
                    .icon(IconName::Info)
                    .tooltip("Environment")
                    .ghost()
                    .selected(self.environment_open)
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(48.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.environment_open = !this.environment_open;
                        cx.notify();
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("right-panel-toggle")
                    .icon(IconName::PanelRight)
                    .tooltip(if self.right_panel_visible {
                        "Hide right panel"
                    } else {
                        "Show right panel"
                    })
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(12.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.right_panel_visible = !this.right_panel_visible;
                        cx.notify();
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("sidebar-collapse-toggle")
                    .icon(IconName::PanelLeft)
                    .tooltip(sidebar_tooltip)
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .left(px(76.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        let inset = if this.sidebar_collapsed {
                            px(110.0)
                        } else {
                            px(14.0)
                        };
                        this.chat_list.update(cx, |chat, cx| {
                            chat.header_left_padding = inset;
                            cx.notify();
                        });
                        cx.notify();
                    }))
            }))
            .children(
                (workspace_page == WorkspacePage::Chat && self.environment_open)
                    .then(|| self.render_environment_popover(cx)),
            )
            .children(self.git_dialog_open.then(|| self.render_git_dialog(cx)))
            .children(self.render_update_notice(cx))
    }
}
