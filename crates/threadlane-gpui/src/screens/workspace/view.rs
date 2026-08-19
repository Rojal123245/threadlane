use std::sync::mpsc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{
    Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
};
use gpui_component::input::{Input, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizableState};
use gpui_component::spinner::Spinner;
use gpui_component::switch::Switch;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};

actions!(threadlane_workspace, [ToggleCommandPalette]);
use threadlane_git::GitStatus;

use crate::app::actions::AppAction;
use crate::app::controller;
use crate::screens::chat::ChatListView;
use crate::screens::right_panel::RightPanelView;
use crate::screens::terminal::TerminalView;
use crate::screens::settings::SettingsView;
use crate::screens::sidebar::SidebarView;
use crate::services::updater::{self, UpdaterEvent};
use crate::state::{AppState, WorkspacePage};
use threadlane_updater::UpdateStatus;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", ToggleCommandPalette, None),
        KeyBinding::new("ctrl-k", ToggleCommandPalette, None),
    ]);
}

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
    terminal: Entity<TerminalView>,
    sidebar_collapsed: bool,
    right_panel_visible: bool,
    bottom_panel_visible: bool,
    environment_open: bool,
    command_palette_open: bool,
    command_palette_selected: usize,
    command_palette_scroll_handle: ScrollHandle,
    command_palette_input: Entity<InputState>,
    git_dialog_open: bool,
    git_include_unstaged: bool,
    git_busy: bool,
    git_message_pending: bool,
    generated_commit_message: Option<String>,
    git_status: Option<GitStatus>,
    git_feedback: Option<String>,
    git_message_input: Entity<InputState>,
    sidebar_resizable_state: Entity<ResizableState>,
    right_panel_resizable_state: Entity<ResizableState>,
    bottom_panel_resizable_state: Entity<ResizableState>,
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
        let terminal_project = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let terminal = cx.new(|cx| TerminalView::new(terminal_project, cx));
        let sidebar_resizable_state = cx.new(|_cx| ResizableState::default());
        let right_panel_resizable_state = cx.new(|_cx| ResizableState::default());
        let bottom_panel_resizable_state = cx.new(|_cx| ResizableState::default());
        let git_message_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let command_palette_scroll_handle = ScrollHandle::new();
        let command_palette_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Type a command or search sessions…")
        });
        let (git_event_tx, git_event_rx) = mpsc::channel();
        let (updater_tx, updater_rx) = mpsc::channel();

        #[cfg(target_os = "macos")]
        if threadlane_updater::is_configured() {
            updater::check(updater_tx.clone());
        }

        let model_clone = model.clone();
        let view = cx.new(|cx| {
            let sub = cx.observe(&model_clone, |_this: &mut Self, _model, cx| {
                cx.notify();
            });

            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let git_events = git_event_rx.try_iter().collect::<Vec<_>>();
                let updater_events = updater_rx.try_iter().collect::<Vec<_>>();
                let _ = this.update(cx, |this, cx| {
                    this.model.update(cx, |state, cx| {
                        if state.apply_session_refreshes() {
                            cx.notify();
                        }
                    });
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
                terminal,
                sidebar_collapsed: false,
                right_panel_visible: false,
                bottom_panel_visible: false,
                environment_open: false,
                command_palette_open: false,
                command_palette_selected: 0,
                command_palette_input,
                command_palette_scroll_handle,
                git_dialog_open: false,
                git_include_unstaged: true,
                git_busy: false,
                git_message_pending: false,
                generated_commit_message: None,
                git_status: None,
                git_feedback: None,
                git_message_input,
                sidebar_resizable_state,
                right_panel_resizable_state,
                bottom_panel_resizable_state,
                git_event_tx,
                updater_tx,
                _subscriptions: vec![sub],
            }
        });

        let view_handle = view.downgrade();
        let shortcut_subscription = cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key.eq_ignore_ascii_case("k")
                && (keystroke.modifiers.platform || keystroke.modifiers.control)
                && !keystroke.modifiers.alt
                && !keystroke.modifiers.shift
            {
                if let Some(view) = view_handle.upgrade() {
                    view.update(cx, |view, cx| {
                        view.toggle_command_palette(&ToggleCommandPalette, window, cx);
                    });
                    cx.stop_propagation();
                }
            }
        });
        view.update(cx, |view, _cx| {
            view._subscriptions.push(shortcut_subscription);
        });
        view
    }

    fn open_git_dialog(&mut self, cx: &mut Context<Self>) {
        self.git_dialog_open = true;
        self.git_feedback = None;
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette_open = !self.command_palette_open;
        self.command_palette_selected = 0;
        if self.command_palette_open {
            self.command_palette_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Executes a command-palette action key. This is the single source of truth
    /// for palette action dispatch, shared by keyboard activation and click.
    fn execute_palette_action(
        &mut self,
        action_key: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = self.model.clone();
        match action_key {
            "new" => {
                model.update(cx, |state, _cx| {
                    controller::dispatch(state, AppAction::BeginNewTask);
                });
            }
            "attach" => {
                cx.spawn(async move |_this, cx| {
                    let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                        return;
                    };
                    let path = folder.path().to_path_buf();
                    let _ = model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::AttachProject(path));
                        cx.notify();
                    });
                })
                .detach();
            }
            "git" => self.open_git_dialog(cx),
            "settings" => {
                model.update(cx, |state, _cx| {
                    controller::dispatch(state, AppAction::OpenSettings);
                });
            }
            "sidebar" => {
                self.sidebar_collapsed = !self.sidebar_collapsed;
            }
            "panel" => {
                self.right_panel_visible = !self.right_panel_visible;
            }
            "goal" | "model" | "compact" => {
                let value = if action_key == "compact" {
                    "/compact".to_string()
                } else {
                    format!("/{action_key} ")
                };
                self.chat_list.update(cx, |chat, cx| {
                    chat.input_state.update(cx, |input, cx| {
                        input.set_value(value, window, cx);
                    });
                });
            }
            _ => {}
        }
        cx.notify();
    }

    fn command_palette_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        if !self.command_palette_open {
            return;
        }
        if key.eq_ignore_ascii_case("escape") {
            self.command_palette_open = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let query = self
            .command_palette_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let commands = [
            ("New Task", "Start a fresh session", "new"),
            (
                "Add Project",
                "Attach a project folder to your workspace",
                "attach",
            ),
            (
                "Goal Planning (/goal)",
                "Autonomous goal loop extension",
                "goal",
            ),
            (
                "Model Selection (/model)",
                "Switch model or provider",
                "model",
            ),
            (
                "Compact History (/compact)",
                "Compact context conversation",
                "compact",
            ),
            (
                "Git Review & Commit",
                "Review changed files and commit",
                "git",
            ),
            (
                "Toggle Sidebar",
                "Show or hide your projects and tasks",
                "sidebar",
            ),
            (
                "Toggle Right Panel",
                "Show review / files / terminal",
                "panel",
            ),
            ("Settings", "Configure API keys and providers", "settings"),
        ];
        let matching: Vec<_> = commands
            .iter()
            .filter(|(name, desc, _)| {
                query.is_empty()
                    || name.to_lowercase().contains(&query)
                    || desc.to_lowercase().contains(&query)
            })
            .collect();
        match key.to_ascii_lowercase().as_str() {
            "arrowdown" | "down" => {
                if !matching.is_empty() {
                    self.command_palette_selected =
                        (self.command_palette_selected + 1) % matching.len();
                    self.command_palette_scroll_handle
                        .scroll_to_item(self.command_palette_selected);
                }
                cx.stop_propagation();
                cx.notify();
            }
            "arrowup" | "up" => {
                if !matching.is_empty() {
                    self.command_palette_selected = self
                        .command_palette_selected
                        .checked_sub(1)
                        .unwrap_or(matching.len() - 1);
                    self.command_palette_scroll_handle
                        .scroll_to_item(self.command_palette_selected);
                }
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                if let Some((_, _, action_key)) = matching.get(self.command_palette_selected) {
                    let action_key = *action_key;
                    self.command_palette_open = false;
                    self.command_palette_selected = 0;
                    self.execute_palette_action(action_key, window, cx);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            _ => {}
        }
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
        // A commit is needed whenever there is staged work, or unstaged work that the
        // "Include unstaged" toggle will stage. This applies to Push too, so "Push only"
        // stages + commits dirty work before pushing when the toggle asks for it.
        let needs_commit = self.git_status.as_ref().is_some_and(|status| {
            status.staged_changes || (self.git_include_unstaged && status.unstaged_changes)
        });
        if needs_commit && message.is_empty() {
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
                let mut status = threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())?;
                if include_unstaged {
                    let mut staged_any = false;
                    for file in status.files.iter().filter(|file| file.unstaged) {
                        threadlane_git::stage_file(&work_dir, &file.path)
                            .map_err(|e| e.to_string())?;
                        staged_any = true;
                    }
                    status.unstaged_changes = false;
                    status.staged_changes |= staged_any;
                }
                // Commit whenever there is staged work left to record. This makes "Push only"
                // honor the include-unstaged toggle: dirty files are staged above and committed
                // here before the push below.
                if status.staged_changes {
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
        let status = self.git_status.as_ref();

        let pr_info = status.and_then(|s| s.pr.clone());

        div()
            .id("environment-popover")
            .absolute()
            .on_mouse_down(MouseButton::Left, |_event, _window, _cx| {})
            .top(px(48.0))
            .right(px(44.0))
            .w(px(290.0))
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            // 1. Header: "Environment" + "+" Button
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("Environment"),
                    )
                    .child(
                        Button::new("env-new-branch-btn")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .tooltip("New branch or worktree")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.environment_open = false;
                                this.open_git_dialog(cx);
                            })),
                    ),
            )
            // 2. Commit or Push Row
            .child(
                div()
                    .id("environment-commit")
                    .h(px(38.0))
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
            // 6. PR Card / Details (if PR detected)
            .children(pr_info.map(|pr| {
                let pr_url = pr.url.clone();
                let pr_num = pr.number;
                let pr_title = pr.title.clone();
                let pr_title_display = if pr.title.is_empty() {
                    format!("PR #{pr_num}")
                } else {
                    pr.title.clone()
                };

                let failing_checks = pr.failing_checks;
                let pending_checks = pr.pending_checks;
                let total_checks = pr.total_checks;
                let comments_count = pr.comments_count;

                let failing_check_names: Vec<String> = pr.checks.iter().filter(|c| {
                    let concl = c.conclusion.as_deref().unwrap_or("").to_uppercase();
                    matches!(concl.as_str(), "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR")
                }).map(|c| c.name.clone()).collect();
                let failed_summary = failing_check_names.join(", ");

                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pt_1()
                    .child(div().h(px(1.0)).w_full().bg(theme.border).my_1())
                    // PR Title Row
                    .child(
                        div()
                            .id("environment-pr-title")
                            .h(px(36.0))
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
                                    .child(Icon::default().path("icons/git/actions.svg")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(pr_title_display),
                            )
                            .child(
                                div()
                                    .size(px(14.0))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::ExternalLink),
                            )
                            .on_click({
                                let target_url = pr_url.clone();
                                move |_event, _window, cx| {
                                    if !target_url.is_empty() {
                                        cx.open_url(&target_url);
                                    }
                                }
                            }),
                    )
                    // CI Checks Row
                    .child(
                        div()
                            .id("environment-pr-ci")
                            .h(px(36.0))
                            .w_full()
                            .px_3()
                            .rounded_lg()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(18.0))
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
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .child(if failing_checks > 0 {
                                        format!("{failing_checks} failing check{}", if failing_checks == 1 { "" } else { "s" })
                                    } else if pending_checks > 0 {
                                        format!("{pending_checks} in progress")
                                    } else {
                                        format!("All {} checks passed", total_checks.max(1))
                                    }),
                            )
                            .child(if failing_checks > 0 {
                                let fix_pr_num = pr_num;
                                let fix_pr_title = pr_title.clone();
                                let fix_failed_summary = failed_summary.clone();
                                Button::new("fix-ci-btn")
                                    .ghost()
                                    .xsmall()
                                    .label("Fix")
                                    .tooltip("Ask AI to fix failing CI checks")
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        this.environment_open = false;
                                        let prompt = format!(
                                            "Please inspect and fix the failing CI check on PR #{fix_pr_num} ({fix_pr_title}): {fix_failed_summary}"
                                        );
                                        this.chat_list.update(cx, |chat, cx| {
                                            chat.set_composer_text(&prompt, window, cx);
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
                    // Comments Row
                    .child(
                        div()
                            .id("environment-pr-comments")
                            .h(px(36.0))
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
                                    .child(IconName::File),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .child(format!("{comments_count} comment{}", if comments_count == 1 { "" } else { "s" })),
                            )
                            .on_click({
                                let comments_pr_num = pr_num;
                                let comments_pr_title = pr_title.clone();
                                cx.listener(move |this, _event, window, cx| {
                                    this.environment_open = false;
                                    let prompt = format!(
                                        "Please review and address comments and feedback on PR #{comments_pr_num} ({comments_pr_title})."
                                    );
                                    this.chat_list.update(cx, |chat, cx| {
                                        chat.set_composer_text(&prompt, window, cx);
                                    });
                                    cx.notify();
                                })
                            }),
                    )
            }))
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
        let has_dirty = |status: &GitStatus| {
            status.staged_changes || (self.git_include_unstaged && status.unstaged_changes)
        };
        let can_commit =
            !self.git_busy && !self.git_message_pending && status.is_some_and(has_dirty);
        let can_push = !self.git_busy
            && !self.git_message_pending
            && status.is_some_and(|status| status.ahead > 0 || has_dirty(status));
        let toggle_view = cx.entity().downgrade();

        Dialog::new(cx)
            .w(px(440.0))
            .overlay(true)
            .overlay_closable(false)
            .close_button(false)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        DialogHeader::new()
                            .relative()
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
                                        DialogTitle::new()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Commit changes"),
                                    )
                                    .child(
                                        DialogDescription::new()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("On branch {branch}")),
                                    ),
                            )
                            .child(
                                Button::new("git-dialog-close")
                                    .absolute()
                                    .top(px(16.0))
                                    .right(px(16.0))
                                    .icon(IconName::Close)
                                    .tooltip("Close")
                                    .ghost()
                                    .xsmall()
                                    .disabled(self.git_busy)
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
                                        Tag::new()
                                            .child(format!("+{additions}"))
                                            .with_variant(TagVariant::Success)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(format!("−{deletions}"))
                                            .with_variant(TagVariant::Danger)
                                            .small(),
                                    )
                                    .child(div().text_color(theme.muted_foreground).child(
                                        format!("{} files", status.map_or(0, |s| s.files.len())),
                                    )),
                            ),
                    )
                    .child(
                        DialogContent::new()
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
                        DialogFooter::new()
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

    fn render_command_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let query = self
            .command_palette_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let model = self.model.clone();
        let state = model.read(cx);

        let mut session_results = Vec::new();
        for project in &state.projects {
            for session in &project.sessions {
                if query.is_empty()
                    || session.title.to_lowercase().contains(&query)
                    || project.name.to_lowercase().contains(&query)
                {
                    session_results.push((
                        project.work_dir.clone(),
                        session.id.clone(),
                        session.title.clone(),
                        project.name.clone(),
                    ));
                }
            }
        }

        let commands = [
            ("New Task", "Start a fresh session", "new"),
            (
                "Add Project",
                "Attach a project folder to your workspace",
                "attach",
            ),
            (
                "Goal Planning (/goal)",
                "Autonomous goal loop extension",
                "goal",
            ),
            (
                "Model Selection (/model)",
                "Switch model or provider",
                "model",
            ),
            (
                "Compact History (/compact)",
                "Compact context conversation",
                "compact",
            ),
            (
                "Git Review & Commit",
                "Review changed files and commit",
                "git",
            ),
            (
                "Toggle Sidebar",
                "Show or hide your projects and tasks",
                "sidebar",
            ),
            (
                "Toggle Right Panel",
                "Show review / files / terminal",
                "panel",
            ),
            ("Settings", "Configure API keys and providers", "settings"),
        ];

        let matching_commands: Vec<_> = commands
            .into_iter()
            .filter(|(name, desc, _)| {
                query.is_empty()
                    || name.to_lowercase().contains(&query)
                    || desc.to_lowercase().contains(&query)
            })
            .collect();
        let index_offset = matching_commands.len();

        div()
            .id("command-palette-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.5))
            .flex()
            .items_start()
            .justify_center()
            .pt(px(80.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    this.command_palette_open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("command-palette-modal")
                    .w(px(560.0))
                    .max_h(px(480.0))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_event, _window, _cx| {})
                    .child(
                        div()
                            .p_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Input::new(&self.command_palette_input).appearance(false)),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("Commands & Actions"),
                    )
                    .child(
                        div()
                            .id("command-palette-container")
                            .relative()
                            .w_full()
                            .max_h(px(420.0))
                            .child(
                                div()
                                    .id("command-palette-results")
                                    .size_full()
                                    .max_h(px(420.0))
                                    .track_scroll(&self.command_palette_scroll_handle)
                                    .overflow_y_scroll()
                                    .py_2()
                                    .children(matching_commands.into_iter().enumerate().map(
                                        |(index, (name, desc, action_key))| {
                                            div()
                                                .id(SharedString::from(format!("palette-cmd-{action_key}")))
                                                .mx_2()
                                                .my_0p5()
                                                .px_3()
                                                .py_2()
                                                .rounded_lg()
                                                .hover(|style| style.bg(theme.list_hover))
                                                .when(index == self.command_palette_selected, |style| {
                                                    style.bg(theme.list_active)
                                                })
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_0p5()
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .font_weight(FontWeight::MEDIUM)
                                                                .child(name),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(theme.muted_foreground)
                                                                .child(desc),
                                                        ),
                                                )
                                                .on_click(cx.listener(move |this, _event, window, cx| {
                                                    this.command_palette_open = false;
                                                    this.command_palette_selected = 0;
                                                    this.execute_palette_action(action_key, window, cx);
                                                }))
                                        },
                                    ))
                                    .when(!session_results.is_empty(), |list| {
                                        list.child(
                                            div()
                                                .mt_2()
                                                .px_3()
                                                .py_1()
                                                .text_xs()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.muted_foreground)
                                                .child("Sessions"),
                                        )
                                        .children(
                                            session_results.into_iter().enumerate().take(8).map(
                                                |(session_idx, (work_dir, session_id, title, project))| {
                                                    let model = self.model.clone();
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "palette-session-{session_id}"
                                                        )))
                                                        .mx_2()
                                                        .my_0p5()
                                                        .px_3()
                                                        .py_2()
                                                        .rounded_lg()
                                                        .hover(|style| style.bg(theme.list_hover))
                                                        .when(
                                                            index_offset + session_idx
                                                                == self.command_palette_selected,
                                                            |style| style.bg(theme.list_active),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex()
                                                                .flex_col()
                                                                .gap_0p5()
                                                                .child(
                                                                    div()
                                                                        .text_sm()
                                                                        .font_weight(FontWeight::MEDIUM)
                                                                        .child(title),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .text_color(theme.muted_foreground)
                                                                        .child(project),
                                                                ),
                                                        )
                                                        .on_click(cx.listener(
                                                            move |this, _event, _window, cx| {
                                                                this.command_palette_open = false;
                                                                let work_dir = work_dir.clone();
                                                                let session_id = session_id.clone();
                                                                model.update(cx, |state, _cx| {
                                                                    controller::dispatch(
                                                                        state,
                                                                        AppAction::SelectSession {
                                                                            work_dir,
                                                                            session_id,
                                                                        },
                                                                    );
                                                                });
                                                                cx.notify();
                                                            },
                                                        ))
                                                },
                                            ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .child(gpui_component::scroll::Scrollbar::vertical(
                                        &self.command_palette_scroll_handle,
                                    )),
                            ),
                    )
            )
            .into_any_element()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(message) = self.generated_commit_message.take() {
            self.git_message_input
                .update(cx, |input, cx| input.set_value(message, window, cx));
        }
        let workspace_page = self.model.read(cx).workspace_page;
        if let Some(project) = self.model.read(cx).active_work_dir.clone() {
            self.terminal.update(cx, |terminal, cx| terminal.set_project(project, cx));
        }
        let theme = cx.theme().colors;
        let sidebar_tooltip = if self.sidebar_collapsed {
            "Show sidebar"
        } else {
            "Collapse sidebar"
        };

        let chat_page_content = {
            let upper_content = if self.right_panel_visible {
                h_resizable("workspace-chat-right-split")
                    .with_state(&self.right_panel_resizable_state)
                    .child(
                        resizable_panel()
                            .child(self.chat_list.clone()),
                    )
                    .child(
                        resizable_panel()
                            .size(px(300.0))
                            .size_range(px(240.0)..px(800.0))
                            .child(self.right_panel.clone()),
                    )
                    .into_any_element()
            } else {
                self.chat_list.clone().into_any_element()
            };

            let main_content = if self.bottom_panel_visible {
                let terminal_panel = div()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(theme.background)
                    .child(
                        div()
                            .h(px(36.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .px_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Terminal"),
                    )
                    .child(div().flex_1().min_h_0().child(self.terminal.clone()));

                v_resizable("workspace-main-bottom-split")
                    .with_state(&self.bottom_panel_resizable_state)
                    .child(
                        resizable_panel()
                            .child(upper_content),
                    )
                    .child(
                        resizable_panel()
                            .size(px(280.0))
                            .size_range(px(120.0)..px(600.0))
                            .child(terminal_panel),
                    )
                    .into_any_element()
            } else {
                upper_content
            };

            if !self.sidebar_collapsed {
                h_resizable("workspace-sidebar-main-split")
                    .with_state(&self.sidebar_resizable_state)
                    .child(
                        resizable_panel()
                            .size(px(240.0))
                            .size_range(px(160.0)..px(500.0))
                            .child(self.sidebar.clone()),
                    )
                    .child(
                        resizable_panel()
                            .child(main_content),
                    )
                    .into_any_element()
            } else {
                main_content
            }
        };

        div()
            .relative()
            .flex()
            .w_full()
            .h_full()
            .on_key_down(cx.listener(Self::command_palette_key_down))
            .on_action(cx.listener(Self::toggle_command_palette))
            .bg(theme.background)
            .child(match workspace_page {
                WorkspacePage::Chat => chat_page_content,
                WorkspacePage::Settings => self.settings.clone().into_any_element(),
            })
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("command-palette-btn")
                    .icon(IconName::SquareTerminal)
                    .tooltip("Command Palette (Cmd+K)")
                    .ghost()
                    .selected(self.command_palette_open)
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(120.0))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.command_palette_open = !this.command_palette_open;
                        this.command_palette_selected = 0;
                        if this.command_palette_open {
                            this.command_palette_input
                                .update(cx, |input, cx| input.focus(window, cx));
                        }
                        cx.notify();
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("bottom-panel-toggle")
                    .icon(if self.bottom_panel_visible {
                        IconName::PanelBottomOpen
                    } else {
                        IconName::PanelBottom
                    })
                    .tooltip(if self.bottom_panel_visible {
                        "Hide terminal"
                    } else {
                        "Show terminal"
                    })
                    .ghost()
                    .selected(self.bottom_panel_visible)
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(84.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.bottom_panel_visible = !this.bottom_panel_visible;
                        cx.notify();
                    }))
            }))
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
                        if this.environment_open {
                            this.refresh_git_status(cx);
                        }
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
                (workspace_page == WorkspacePage::Chat && self.environment_open).then(|| {
                    div()
                        .id("environment-backdrop")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.environment_open = false;
                                cx.notify();
                            }),
                        )
                }),
            )
            .children(
                (workspace_page == WorkspacePage::Chat && self.environment_open)
                    .then(|| self.render_environment_popover(cx)),
            )
            .children(self.git_dialog_open.then(|| self.render_git_dialog(cx)))
            .children(
                self.command_palette_open
                    .then(|| self.render_command_palette(cx)),
            )
            .children(self.render_update_notice(cx))
    }
}
