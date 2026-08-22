use gpui::prelude::FluentBuilder;
use gpui::InteractiveElement;
use gpui::*;

use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Icon, IconName, Sizable, WindowExt};

use crate::app::{actions::AppAction, controller};
use crate::state::{AppState, SessionHealth, SessionInfo, TrajectoryEntry};

fn safe_file_stem(title: &str) -> String {
    let stem = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        "threadlane-session".into()
    } else {
        stem.into()
    }
}

fn read_jsonl_for_export(path: &std::path::Path) -> Result<Vec<serde_json::Value>, String> {
    let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    Ok(contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(
            |(index, line)| match serde_json::from_str::<serde_json::Value>(line) {
                Ok(record) => serde_json::json!({ "line": index + 1, "record": record }),
                Err(error) => serde_json::json!({
                    "line": index + 1,
                    "raw": line,
                    "parse_error": error.to_string(),
                }),
            },
        )
        .collect())
}

fn build_diagnostic_export(
    session_file: &std::path::Path,
    session_id: &str,
    title: &str,
    work_dir: &std::path::Path,
    runtime: Option<&crate::services::sessions::SessionRuntime>,
    trajectory: Vec<TrajectoryEntry>,
    include_log: bool,
) -> Result<serde_json::Value, String> {
    let exported_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let selected_model = runtime.map(|runtime| runtime.selected_model.clone());
    let system_prompt = runtime.map(|runtime| runtime.system_prompt.clone());
    let harness_error = runtime.and_then(|runtime| runtime.harness_error.clone());
    let runtime_status = runtime.map(|runtime| format!("{:?}", runtime.status()));
    let log = if include_log {
        let canonical = read_jsonl_for_export(session_file)?;
        Some(serde_json::json!({
            "canonical_records": canonical,
        }))
    } else {
        None
    };

    Ok(serde_json::json!({
        "schema_version": 1,
        "exported_at_unix": exported_at_unix,
        "session": {
            "id": session_id,
            "title": title,
            "project_root": work_dir.display().to_string(),
            "session_file": session_file.display().to_string(),
            "selected_model": selected_model,
            "runtime_status": runtime_status,
            "harness_error": harness_error,
        },
        "system_prompt": system_prompt,
        "trajectory": trajectory,
        "session_log": log,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateGroup {
    Today,
    Yesterday,
    ThisWeek,
    Older,
}

impl DateGroup {
    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This Week",
            Self::Older => "Older",
        }
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Callers pass a shared `now` so a render pass performs one clock read
/// instead of one per row.
fn get_date_group(timestamp: u64, now: u64) -> DateGroup {
    let seconds = now.saturating_sub(timestamp);
    if seconds < 86400 {
        DateGroup::Today
    } else if seconds < 172800 {
        DateGroup::Yesterday
    } else if seconds < 604800 {
        DateGroup::ThisWeek
    } else {
        DateGroup::Older
    }
}

fn format_time_ago(timestamp: u64, now: u64) -> String {
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        0..=59 => format!("{}s ago", seconds),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

pub struct SidebarView {
    model: Entity<AppState>,
    search_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));

        let sub1 = cx.observe(&model, |_this, _model, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let sub2 = cx.subscribe_in(
            &search_input,
            window,
            move |_this, search_input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = search_input.read(cx).value().to_string();
                    model_clone.update(cx, |state, cx| {
                        state.search_query = query;
                        cx.notify();
                    });
                }
            },
        );

        Self {
            model,
            search_input,
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pt(px(48.0))
            .pb_3()
            .bg(theme.title_bar)
            .child(
                Button::new("new-task-btn")
                    .icon(IconName::Plus)
                    .label("New Task")
                    .ghost()
                    .w_full()
                    .on_click(move |_event, _window, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::BeginNewTask);
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(36.0))
                    .rounded_md()
                    .text_color(theme.muted_foreground)
                    .child(IconName::Search)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.search_input)
                                .appearance(false)
                                .bordered(false),
                        ),
                    ),
            )
    }

    fn render_history_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let theme = cx.theme().colors;
        let session_count = self
            .model
            .read(cx)
            .projects
            .iter()
            .map(|project| project.sessions.len())
            .sum::<usize>();

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .pt_3()
            .pb_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("RECENT"),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(theme.secondary)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(session_count.to_string()),
                    ),
            )
            .child(
                Button::new("attach-project-btn")
                    .icon(IconName::Folder)
                    .tooltip("Attach Project")
                    .ghost()
                    .xsmall()
                    .on_click(move |_event, _window, cx| {
                        let model = model.clone();
                        cx.spawn(async move |cx| {
                            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await
                            else {
                                return;
                            };
                            let path = folder.path().to_path_buf();
                            let _ = model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::AttachProject(path));
                                cx.notify();
                            });
                        })
                        .detach();
                    }),
            )
    }

    fn render_session_card(
        &self,
        session: &SessionInfo,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let health = session.health.clone();
        let is_generating = is_active && self.model.read(cx).is_generating;
        let is_working = health == SessionHealth::Working || is_generating;

        let status_indicator = if is_working {
            Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_1p5()
                    .py(px(0.5))
                    .rounded_full()
                    .bg(theme.primary.opacity(0.15))
                    .child(gpui_component::spinner::Spinner::new().xsmall())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.primary)
                            .child("Running"),
                    )
                    .into_any_element(),
            )
        } else {
            match health {
                SessionHealth::Warning => Some(
                    Tag::new()
                        .child("!")
                        .with_variant(TagVariant::Warning)
                        .small()
                        .into_any_element(),
                ),
                SessionHealth::Working | SessionHealth::Healthy => None,
            }
        };

        let bg_color = if is_active {
            theme.sidebar_accent
        } else {
            gpui::transparent_black()
        };

        let border_color = if is_active {
            theme.border.opacity(0.4)
        } else {
            gpui::transparent_black()
        };

        let title_color = if is_active {
            theme.foreground
        } else {
            theme.sidebar_foreground
        };

        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        let model = self.model.clone();
        let context_work_dir = session.work_dir.clone();
        let context_session_id = session.id.clone();
        let context_model = self.model.clone();
        let copy_session_file = session.session_file.display().to_string();
        let export_log_source = session.session_file.clone();
        let export_trajectory_title = session.title.clone();
        let quick_settle_model = self.model.clone();
        let quick_settle_work_dir = session.work_dir.clone();
        let quick_settle_session_id = session.id.clone();
        let time_ago = format_time_ago(session.updated_at, now_unix_secs());
        let project = session
            .work_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".to_string());

        let pr_info = self
            .model
            .read(cx)
            .git_statuses
            .get(&session.work_dir)
            .and_then(|g| g.pr.as_ref())
            .cloned();

        let pr_meta = pr_info.map(|pr| {
            let state_upper = pr.state.to_uppercase();
            let is_merged = state_upper == "MERGED";
            let is_draft = pr.is_draft || state_upper == "DRAFT";
            let is_closed = state_upper == "CLOSED";

            let (pr_bg, pr_fg, pr_label) = if is_merged {
                (
                    theme.accent.opacity(0.15),
                    theme.accent,
                    format!("#{} merged", pr.number),
                )
            } else if is_draft {
                (
                    theme.secondary,
                    theme.muted_foreground,
                    format!("#{} draft", pr.number),
                )
            } else if is_closed {
                (
                    theme.danger.opacity(0.12),
                    theme.danger,
                    format!("#{} closed", pr.number),
                )
            } else {
                (
                    theme.primary.opacity(0.12),
                    theme.primary,
                    format!("#{}", pr.number),
                )
            };

            let ci_chip = if pr.failing_checks > 0 {
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0))
                        .px_1()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(theme.danger.opacity(0.12))
                        .text_color(theme.danger)
                        .child(IconName::Close)
                        .child(format!("{} fail", pr.failing_checks)),
                )
            } else if pr.pending_checks > 0 {
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0))
                        .px_1()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(theme.warning.opacity(0.12))
                        .text_color(theme.warning)
                        .child(IconName::Asterisk)
                        .child(format!("{}/{}", pr.passing_checks, pr.total_checks)),
                )
            } else if pr.total_checks > 0 {
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0))
                        .px_1()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(theme.success.opacity(0.12))
                        .text_color(theme.success)
                        .child(IconName::CircleCheck)
                        .child(format!("{}/{}", pr.passing_checks, pr.total_checks)),
                )
            } else {
                None
            };

            let comments_chip = (pr.comments_count > 0).then(|| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.0))
                    .px_1()
                    .py(px(0.5))
                    .rounded(px(3.0))
                    .bg(theme.secondary)
                    .text_color(theme.muted_foreground)
                    .child(
                        svg()
                            .path("icons/git/comments.svg")
                            .size(px(11.0))
                            .text_color(theme.muted_foreground),
                    )
                    .child(format!("{}", pr.comments_count))
            });

            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(pr_bg)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(pr_fg)
                        .child(pr_label),
                )
                .children(ci_chip)
                .children(comments_chip)
        });

        let mut row2_items = Vec::new();
        row2_items.push(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .text_color(theme.muted_foreground)
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
                        .text_color(theme.muted_foreground.opacity(0.6)),
                )
                .child(div().max_w(px(110.0)).truncate().child(project))
                .into_any_element(),
        );

        if let Some(pr_chips) = pr_meta {
            row2_items.push(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground.opacity(0.4))
                    .child("•")
                    .into_any_element(),
            );
            row2_items.push(pr_chips.into_any_element());
        }

        div()
            .id(SharedString::from(format!("session-card-{}", session.id)))
            .group("session-card")
            .relative()
            .flex()
            .items_stretch()
            .mx_2()
            .my(px(1.5))
            .rounded_md()
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .hover(|style| {
                style.bg(if is_active {
                    theme.sidebar_accent
                } else {
                    theme.list_hover
                })
            })
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                let work_dir = work_dir.clone();
                let session_id = session_id.clone();
                model.update(cx, |state, cx| {
                    controller::dispatch(
                        state,
                        AppAction::SelectSession {
                            work_dir,
                            session_id,
                        },
                    );
                    cx.notify();
                });
            })
            .when(is_active, |this| {
                this.child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(4.0))
                        .bottom(px(4.0))
                        .w(px(2.5))
                        .rounded_r_full()
                        .bg(theme.primary),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .px_2p5()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(if is_active {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .text_color(title_color)
                                    .truncate()
                                    .child(session.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .children(status_indicator)
                                    .when(!is_working, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(time_ago),
                                        )
                                    })
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "settle-session-{}",
                                            session.id
                                        )))
                                        .icon(IconName::Check)
                                        .ghost()
                                        .xsmall()
                                        .compact()
                                        .opacity(0.0)
                                        .group_hover("session-card", |style| style.opacity(1.0))
                                        .tooltip("Archive session")
                                        .on_click(
                                            move |_event, _window, cx| {
                                                quick_settle_model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::SettleSession {
                                                            work_dir: quick_settle_work_dir.clone(),
                                                            session_id: quick_settle_session_id
                                                                .clone(),
                                                        },
                                                    );
                                                    cx.notify();
                                                });
                                            },
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_xs()
                            .min_w_0()
                            .overflow_hidden()
                            .children(row2_items),
                    ),
            )
            .context_menu(move |menu, _window, _cx| {
                let open_model = context_model.clone();
                let open_work_dir = context_work_dir.clone();
                let open_session_id = context_session_id.clone();
                let copy_session_id = context_session_id.clone();
                let copy_project_path = context_work_dir.to_string_lossy().into_owned();
                let copy_session_file = copy_session_file.clone();
                let export_log_model = context_model.clone();
                let export_log_source = export_log_source.clone();
                let export_log_session_id = context_session_id.clone();
                let export_log_title = export_trajectory_title.clone();
                let export_log_work_dir = context_work_dir.clone();
                let export_trajectory_model = context_model.clone();
                let export_trajectory_source = export_log_source.clone();
                let export_trajectory_session_id = context_session_id.clone();
                let export_trajectory_title = export_trajectory_title.clone();
                let export_trajectory_work_dir = context_work_dir.clone();
                let settle_model = context_model.clone();
                let settle_work_dir = context_work_dir.clone();
                let settle_session_id = context_session_id.clone();
                let remove_model = context_model.clone();
                let remove_work_dir = context_work_dir.clone();
                let remove_session_id = context_session_id.clone();

                menu.item(PopupMenuItem::new("Open Session").on_click(
                    move |_event, _window, cx| {
                        open_model.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                AppAction::SelectSession {
                                    work_dir: open_work_dir.clone(),
                                    session_id: open_session_id.clone(),
                                },
                            );
                            cx.notify();
                        });
                    },
                ))
                .item(
                    PopupMenuItem::new("Copy Session ID").on_click(move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_session_id.clone()));
                    }),
                )
                .item(PopupMenuItem::new("Copy Project Root Path").on_click(
                    move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_project_path.clone()));
                    },
                ))
                .item(PopupMenuItem::new("Copy Session File Path").on_click(
                    move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_session_file.clone()));
                    },
                ))
                .separator()
                .item(PopupMenuItem::new("Export Session Log…").on_click(
                    move |_event, _window, cx| {
                        let model = export_log_model.clone();
                        let source = export_log_source.clone();
                        let session_id = export_log_session_id.clone();
                        let title = export_log_title.clone();
                        let work_dir = export_log_work_dir.clone();
                        let (trajectory, runtime) = model.update(cx, |state, _cx| {
                            (
                                state.session_trajectory(&session_id).to_vec(),
                                Some(
                                    state.ensure_session_runtime(work_dir.clone(), source.clone()),
                                ),
                            )
                        });
                        cx.spawn(async move |cx| {
                            let default_name =
                                format!("{}-session-diagnostics.json", safe_file_stem(&title));
                            let Some(destination) = rfd::AsyncFileDialog::new()
                                .set_file_name(&default_name)
                                .save_file()
                                .await
                            else {
                                return;
                            };
                            let result = build_diagnostic_export(
                                &source,
                                &session_id,
                                &title,
                                &work_dir,
                                runtime.as_deref(),
                                trajectory,
                                true,
                            )
                            .and_then(|value| {
                                serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
                            })
                            .and_then(|bytes| {
                                std::fs::write(destination.path(), bytes)
                                    .map_err(|error| error.to_string())
                            });
                            let _ = model.update(cx, |state, cx| {
                                state.session_status = Some(match result {
                                    Ok(()) => "Session diagnostics exported".into(),
                                    Err(error) => {
                                        format!("Could not export session diagnostics: {error}")
                                    }
                                });
                                cx.notify();
                            });
                        })
                        .detach();
                    },
                ))
                .item(PopupMenuItem::new("Export Trajectory…").on_click(
                    move |_event, _window, cx| {
                        let model = export_trajectory_model.clone();
                        let session_id = export_trajectory_session_id.clone();
                        let title = export_trajectory_title.clone();
                        let source = export_trajectory_source.clone();
                        let work_dir = export_trajectory_work_dir.clone();
                        let (trajectory, runtime) = model.update(cx, |state, _cx| {
                            (
                                state.session_trajectory(&session_id).to_vec(),
                                Some(
                                    state.ensure_session_runtime(work_dir.clone(), source.clone()),
                                ),
                            )
                        });
                        cx.spawn(async move |cx| {
                            let default_name =
                                format!("{}-trajectory.json", safe_file_stem(&title));
                            let Some(destination) = rfd::AsyncFileDialog::new()
                                .set_file_name(&default_name)
                                .save_file()
                                .await
                            else {
                                return;
                            };
                            let result = build_diagnostic_export(
                                &source,
                                &session_id,
                                &title,
                                &work_dir,
                                runtime.as_deref(),
                                trajectory,
                                false,
                            )
                            .and_then(|value| {
                                serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
                            })
                            .and_then(|bytes| {
                                std::fs::write(destination.path(), bytes)
                                    .map_err(|error| error.to_string())
                            });
                            let _ = model.update(cx, |state, cx| {
                                state.session_status = Some(match result {
                                    Ok(()) => "Trajectory exported".into(),
                                    Err(error) => format!("Could not export trajectory: {error}"),
                                });
                                cx.notify();
                            });
                        })
                        .detach();
                    },
                ))
                .separator()
                .item(
                    PopupMenuItem::new("Archive Session").on_click(move |_event, window, cx| {
                        let model = settle_model.clone();
                        let work_dir = settle_work_dir.clone();
                        let session_id = settle_session_id.clone();
                        window.open_alert_dialog(cx, {
                            let model = model.clone();
                            let work_dir = work_dir.clone();
                            let session_id = session_id.clone();
                            move |alert, _window, _cx| {
                                let model = model.clone();
                                let work_dir = work_dir.clone();
                                let session_id = session_id.clone();
                                alert
                                    .title("Archive session?")
                                    .description(format!(
                                        "This removes session {session_id} from the active list."
                                    ))
                                    .show_cancel(true)
                                    .on_ok(move |_event, _window, cx| {
                                        model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::SettleSession {
                                                    work_dir: work_dir.clone(),
                                                    session_id: session_id.clone(),
                                                },
                                            );
                                            cx.notify();
                                        });
                                        true
                                    })
                            }
                        });
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Remove Session").on_click(move |_event, window, cx| {
                        let model = remove_model.clone();
                        let work_dir = remove_work_dir.clone();
                        let session_id = remove_session_id.clone();
                        window.open_alert_dialog(cx, {
                            let model = model.clone();
                            let work_dir = work_dir.clone();
                            let session_id = session_id.clone();
                            move |alert, _window, _cx| {
                                let model = model.clone();
                                let work_dir = work_dir.clone();
                                let session_id = session_id.clone();
                                alert
                                    .title("Remove session?")
                                    .description(format!(
                                        "This permanently removes session {session_id}."
                                    ))
                                    .button_props(
                                        DialogButtonProps::default()
                                            .ok_text("Remove")
                                            .ok_variant(ButtonVariant::Danger)
                                            .show_cancel(true),
                                    )
                                    .on_ok(move |_event, _window, cx| {
                                        model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::RemoveSession {
                                                    work_dir: work_dir.clone(),
                                                    session_id: session_id.clone(),
                                                },
                                            );
                                            cx.notify();
                                        });
                                        true
                                    })
                            }
                        });
                    }),
                )
            })
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let theme = cx.theme().colors;

        div().flex_none().px_3().py_2().child(
            Button::new("sidebar-settings")
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_start()
                        .gap_2()
                        .child(IconName::Settings)
                        .child("Settings"),
                )
                .ghost()
                .w_full()
                .justify_start()
                .text_color(theme.muted_foreground)
                .on_click(move |_event, _window, cx| {
                    model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::OpenSettings);
                        cx.notify();
                    });
                }),
        )
    }

    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let state = self.model.read(cx);
        let query = state.search_query.trim().to_lowercase();
        let active_work_dir = state.active_work_dir.clone();
        let active_session_id = state.active_session_id.clone();
        let now = now_unix_secs();

        // Filter by reference and clone only rows that will render. Grouping
        // before sorting is equivalent to the previous global sort because
        // bucket insertion preserves scan order.
        let mut grouped: Vec<(DateGroup, Vec<SessionInfo>)> = vec![
            (DateGroup::Today, Vec::new()),
            (DateGroup::Yesterday, Vec::new()),
            (DateGroup::ThisWeek, Vec::new()),
            (DateGroup::Older, Vec::new()),
        ];
        for session in state
            .projects
            .iter()
            .flat_map(|project| project.sessions.iter())
        {
            if !query.is_empty()
                && !session.title.to_lowercase().contains(&query)
                && !session.id.to_lowercase().contains(&query)
            {
                continue;
            }
            let mut session = session.clone();
            if state.session_is_generating(&session.session_file) {
                session.health = SessionHealth::Working;
            }
            let group = get_date_group(session.updated_at, now);
            if let Some((_, entries)) = grouped
                .iter_mut()
                .find(|(candidate, _)| *candidate == group)
            {
                entries.push(session);
            }
        }
        for (_, entries) in grouped.iter_mut() {
            entries.sort_by(|left, right| {
                right
                    .updated_at
                    .cmp(&left.updated_at)
                    .then_with(|| left.title.cmp(&right.title))
            });
        }

        let mut children = Vec::new();
        for (group, sessions) in grouped {
            if sessions.is_empty() {
                continue;
            }
            children.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt(if group == DateGroup::Today {
                        px(4.0)
                    } else {
                        px(12.0)
                    })
                    .pb_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground.opacity(0.8))
                            .child(group.label()),
                    )
                    .child(div().h(px(1.0)).flex_1().bg(theme.border.opacity(0.35)))
                    .into_any_element(),
            );
            for session in sessions {
                let is_active = active_work_dir.as_ref() == Some(&session.work_dir)
                    && active_session_id.as_deref() == Some(session.id.as_str());
                children.push(
                    self.render_session_card(&session, is_active, cx)
                        .into_any_element(),
                );
            }
        }

        if children.is_empty() {
            children.push(
                div()
                    .px_4()
                    .py_6()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if query.is_empty() {
                        "No tasks yet. Start a new task above."
                    } else {
                        "No matching tasks."
                    })
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .children(children)
            .into_any_element()
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.title_bar)
            .child(self.render_header(cx))
            .child(self.render_history_header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .pb_3()
                    .child(self.render_history(cx)),
            )
            .child(self.render_footer(cx))
    }
}
