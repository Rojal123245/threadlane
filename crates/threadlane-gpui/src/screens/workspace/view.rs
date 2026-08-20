use std::sync::mpsc;
use std::time::Duration;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizableState};
use gpui_component::status_bar::StatusBar;
use gpui_component::{
    v_flex, ActiveTheme, Icon, IconName, Root, Selectable, Sizable,
};

actions!(threadlane_workspace, [ToggleCommandPalette]);
use threadlane_git::GitStatus;

use crate::app::actions::AppAction;
use crate::app::controller;
use crate::screens::chat::ChatListView;
use crate::screens::right_panel::RightPanelView;
use crate::screens::settings::SettingsView;
use crate::screens::sidebar::SidebarView;
use crate::screens::terminal::TerminalView;
use crate::services::updater::{self, UpdaterEvent};
use crate::state::{
    compute_full_session_projection, AppState, SessionHydrationRequest, WorkspacePage,
};
use threadlane_updater::UpdateStatus;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", ToggleCommandPalette, None),
        KeyBinding::new("ctrl-k", ToggleCommandPalette, None),
    ]);
}

enum GitEvent {
    Loaded(Result<GitStatus, String>),
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
    command_state: Entity<CommandState>,
    git_status: Option<GitStatus>,
    sidebar_resizable_state: Entity<ResizableState>,
    right_panel_resizable_state: Entity<ResizableState>,
    bottom_panel_resizable_state: Entity<ResizableState>,
    git_event_tx: mpsc::Sender<GitEvent>,
    updater_tx: mpsc::Sender<UpdaterEvent>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub(crate) fn spawn_session_hydration(
        model: Entity<AppState>,
        request: SessionHydrationRequest,
        cx: &mut AsyncApp,
    ) {
        cx.spawn(async move |cx| {
            let session_file = request.session_file.clone();
            let result = cx
                .background_executor()
                .spawn(async move { compute_full_session_projection(&session_file) })
                .await;
            let _ = model.update(cx, |state, cx| {
                if state.active_session_id.as_deref() != Some(&request.session_id) {
                    return;
                }
                match result {
                    Ok(result) => {
                        state.apply_session_hydration(
                            &request.session_id,
                            &request.session_file,
                            result,
                        );
                        state.session_status = state.session_status_for_file(&request.session_file);
                    }
                    Err(error) => state.session_status = Some(format!("Could not load session: {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| AppState::load());
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model.clone(), window, cx));
        let settings = cx.new(|cx| SettingsView::new(model.clone(), window, cx));
        let right_panel = cx.new(|cx| RightPanelView::new(model.clone(), window, cx));
        let terminal_project =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let terminal = cx.new(|cx| TerminalView::new(terminal_project, cx));
        let sidebar_resizable_state = cx.new(|_cx| ResizableState::default());
        let right_panel_resizable_state = cx.new(|_cx| ResizableState::default());
        let bottom_panel_resizable_state = cx.new(|_cx| ResizableState::default());
        let command_state = cx.new(|cx| CommandState::new(window, cx));
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
                let hydration_requests = this
                    .update(cx, |this, cx| {
                        this.model.update(cx, |state, _cx| {
                            std::mem::take(&mut state.pending_hydrations)
                        })
                    })
                    .unwrap_or_default();
                for request in hydration_requests {
                    let model = this
                        .update(cx, |this, _cx| this.model.clone())
                        .ok();
                    if let Some(model) = model {
                        Self::spawn_session_hydration(model, request, cx);
                    }
                }
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
                command_state,
                git_status: None,
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
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_review(cx);
        });
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
        if self.command_palette_open {
            self.command_state.update(cx, |state, cx| {
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
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

    fn refresh_git_status(&mut self, cx: &App) {
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            self.git_status = None;
            return;
        };
        let tx = self.git_event_tx.clone();
        std::thread::spawn(move || {
            let result = threadlane_git::inspect(&work_dir).map_err(|error| error.to_string());
            let _ = tx.send(GitEvent::Loaded(result));
        });
    }

    fn apply_git_event(&mut self, event: GitEvent, _cx: &mut Context<Self>) {
        match event {
            GitEvent::Loaded(Ok(status)) => {
                self.git_status = Some(status);
            }
            GitEvent::Loaded(Err(_)) => {
                self.git_status = None;
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
        let model = self.model.clone();
        let state = model.read(cx);

        let commands: [(&str, &str, &str, IconName, &[&str]); 9] = [
            (
                "New Task",
                "Start a fresh session",
                "new",
                IconName::Plus,
                &["task", "fresh", "session", "new"],
            ),
            (
                "Add Project",
                "Attach a project folder to your workspace",
                "attach",
                IconName::FolderOpen,
                &["folder", "workspace", "attach", "open", "project"],
            ),
            (
                "Goal Planning (/goal)",
                "Autonomous goal loop extension",
                "goal",
                IconName::Bot,
                &["goal", "planning", "loop", "agent", "autonomous"],
            ),
            (
                "Model Selection (/model)",
                "Switch model or provider",
                "model",
                IconName::Cpu,
                &["model", "llm", "switch", "provider", "select"],
            ),
            (
                "Compact History (/compact)",
                "Compact context conversation",
                "compact",
                IconName::Minimize,
                &["compact", "history", "context", "clean"],
            ),
            (
                "Git Review & Commit",
                "Review changed files and commit",
                "git",
                IconName::Github,
                &["git", "diff", "review", "commit", "stage"],
            ),
            (
                "Toggle Sidebar",
                "Show or hide your projects and tasks",
                "sidebar",
                IconName::PanelLeft,
                &["sidebar", "toggle", "hide", "show", "projects"],
            ),
            (
                "Toggle Right Panel",
                "Show review / files / terminal",
                "panel",
                IconName::PanelRight,
                &["panel", "right", "terminal", "review", "toggle"],
            ),
            (
                "Settings",
                "Configure API keys and providers",
                "settings",
                IconName::Settings,
                &["settings", "keys", "provider", "preferences", "config"],
            ),
        ];

        let mut commands_group = CommandGroup::new().label("Commands & Actions");
        for (name, desc, _action_key, icon, keywords) in &commands {
            let name_str = name.to_string();
            let desc_str = desc.to_string();
            let item = CommandItem::new()
                .label(*name)
                .icon(icon.clone())
                .keywords(keywords.iter().copied())
                .child(move |_window, cx| {
                    let colors = cx.theme().colors;
                    v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(name_str.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child(desc_str.clone()),
                        )
                });
            commands_group = commands_group.item(item);
        }

        let mut session_entries = Vec::new();
        let mut sessions_group = CommandGroup::new().label("Sessions");
        for project in &state.projects {
            for session in &project.sessions {
                session_entries.push((project.work_dir.clone(), session.id.clone()));
                let title = session.title.clone();
                let project_name = project.name.clone();
                let item = CommandItem::new()
                    .label(title.clone())
                    .icon(IconName::SquareTerminal)
                    .keywords([project.name.clone(), session.id.clone()])
                    .child(move |_window, cx| {
                        let colors = cx.theme().colors;
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_foreground)
                                    .child(project_name.clone()),
                            )
                    });
                sessions_group = sessions_group.item(item);
            }
        }

        let view = cx.weak_entity();
        let view_cancel = cx.weak_entity();

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
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_event, _window, _cx| {})
                    .child(
                        Command::new(&self.command_state)
                            .bordered(false)
                            .placeholder("Type a command or search sessions…")
                            .max_h(px(420.0))
                            .group(commands_group)
                            .group(sessions_group)
                            .on_cancel(move |_window, cx| {
                                let _ = view_cancel.update(cx, |this, cx| {
                                    this.command_palette_open = false;
                                    cx.notify();
                                });
                            })
                            .on_confirm(move |index, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.command_palette_open = false;
                                    if index.section == 0 {
                                        if let Some((_, _, action_key, _, _)) = commands.get(index.row) {
                                            this.execute_palette_action(action_key, window, cx);
                                        }
                                    } else if index.section == 1 {
                                        if let Some((work_dir, session_id)) = session_entries.get(index.row) {
                                            let work_dir = work_dir.clone();
                                            let session_id = session_id.clone();
                                            this.model.update(cx, |state, _cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::SelectSession {
                                                        work_dir,
                                                        session_id,
                                                    },
                                                );
                                            });
                                        }
                                    }
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let theme = cx.theme().colors;

        let branch = self
            .git_status
            .as_ref()
            .and_then(|s| s.branch.as_deref())
            .unwrap_or("main");
        let (additions, deletions) = self.git_status.as_ref().map_or((0, 0), |s| {
            s.files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
        });
        let dirty_count = self.git_status.as_ref().map_or(0, |s| s.files.len());

        let model_name = if state.selected_model.is_empty() {
            "default"
        } else {
            &state.selected_model
        };

        let active_project = state
            .active_work_dir
            .as_ref()
            .and_then(|wd| {
                state
                    .projects
                    .iter()
                    .find(|p| &p.work_dir == wd)
                    .map(|p| p.name.clone())
            })
            .or_else(|| {
                state
                    .active_work_dir
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "No Project".into());

        StatusBar::new()
            .left(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("status-git-branch")
                            .icon(IconName::Github)
                            .label(format!("{active_project} · {branch}"))
                            .ghost()
                            .xsmall()
                            .tooltip("Git Review & Commit")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.open_git_dialog(cx);
                            })),
                    )
                    .children((dirty_count > 0).then(|| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .children((additions > 0).then(|| {
                                div()
                                    .text_color(theme.success)
                                    .child(format!("+{additions}"))
                            }))
                            .children((deletions > 0).then(|| {
                                div()
                                    .text_color(theme.danger)
                                    .child(format!("−{deletions}"))
                            }))
                    })),
            )
            .right(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(theme.muted)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Cpu).xsmall())
                            .child(model_name.to_string()),
                    )
                    .child(
                        Button::new("status-terminal-toggle")
                            .icon(if self.bottom_panel_visible {
                                IconName::PanelBottomOpen
                            } else {
                                IconName::PanelBottom
                            })
                            .label("Terminal")
                            .ghost()
                            .selected(self.bottom_panel_visible)
                            .xsmall()
                            .tooltip(if self.bottom_panel_visible {
                                "Hide terminal"
                            } else {
                                "Show terminal"
                            })
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.bottom_panel_visible = !this.bottom_panel_visible;
                                cx.notify();
                            })),
                    ),
            )
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace_page = self.model.read(cx).workspace_page;
        if let Some(project) = self.model.read(cx).active_work_dir.clone() {
            self.terminal
                .update(cx, |terminal, cx| terminal.set_project(project, cx));
        }

        let _is_macos = cfg!(target_os = "macos");
        let sidebar_tooltip = if self.sidebar_collapsed {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        };
        let theme = cx.theme().colors;

        let chat_page_content = {
            let upper_content = if self.right_panel_visible {
                h_resizable("workspace-chat-right-split")
                    .with_state(&self.right_panel_resizable_state)
                    .child(resizable_panel().child(self.chat_list.clone()))
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
                    .child(resizable_panel().child(upper_content))
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
                    .child(resizable_panel().child(main_content))
                    .into_any_element()
            } else {
                main_content
            }
        };

        let page_content = match workspace_page {
            WorkspacePage::Chat => chat_page_content.into_any_element(),
            WorkspacePage::Settings => self.settings.clone().into_any_element(),
        };

        let view_with_status_bar = div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(page_content))
            .child(self.render_status_bar(cx));

        div()
            .relative()
            .flex()
            .w_full()
            .h_full()
            .on_action(cx.listener(Self::toggle_command_palette))
            .bg(theme.background)
            .child(view_with_status_bar)
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("command-palette-btn")
                    .icon(IconName::SquareTerminal)
                    .tooltip("Command Palette (Cmd+K)")
                    .ghost()
                    .selected(self.command_palette_open)
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(84.0))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.command_palette_open = !this.command_palette_open;
                        if this.command_palette_open {
                            this.command_state.update(cx, |state, cx| {
                                state.set_query("", window, cx);
                                state.focus(window, cx);
                            });
                        }
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
            .children(
                self.command_palette_open
                    .then(|| self.render_command_palette(cx)),
            )
            .children(self.render_update_notice(cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
    }
}
