use gpui::InteractiveElement;
use gpui::*;

use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::theme::ActiveTheme;
use gpui_component::IconName;

use crate::app::{actions::AppAction, controller};
use crate::state::{AppState, SessionHealth, SessionInfo};

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

fn get_date_group(timestamp: u64) -> DateGroup {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
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

fn format_time_ago(timestamp: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        0..=59 => format!("{}s ago", seconds),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

pub struct SidebarView {
    pub model: Entity<AppState>,
    pub search_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                    model_clone.update(cx, |state, _cx| {
                        state.search_query = query;
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
                div()
                    .id("new-task-btn")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(36.0))
                    .px_2()
                    .rounded_md()
                    .text_color(theme.sidebar_foreground)
                    .w_full()
                    .hover(|style| style.bg(theme.list_hover))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        model.update(cx, |state, _cx| {
                            controller::dispatch(state, AppAction::CreateSession);
                        });
                    })
                    .child(IconName::Plus)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("New Task"),
                    ),
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
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .pt_2()
            .pb_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child("Today"),
            )
            .child(
                div()
                    .id("attach-project-btn")
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_lg()
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.bg(theme.list_hover).text_color(theme.foreground))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            model.update(cx, |state, _cx| {
                                controller::dispatch(state, AppAction::AttachProject(path));
                            });
                        }
                    })
                    .child(IconName::Folder),
            )
    }

    fn render_session_card(
        &self,
        session: &SessionInfo,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let status_indicator = match session.health {
            SessionHealth::Working => Some((theme.info, "◔")),
            SessionHealth::Warning => Some((theme.warning, "▲")),
            SessionHealth::Healthy => None,
        };

        let bg_color = if is_active {
            theme.secondary_hover
        } else {
            theme.title_bar
        };

        let border_color = if is_active {
            theme.secondary_hover
        } else {
            theme.title_bar
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
        let time_ago = format_time_ago(session.updated_at);
        let status = if session.health == SessionHealth::Working {
            format!("Working for {}", time_ago.trim_end_matches(" ago"))
        } else {
            time_ago
        };
        let project = session
            .work_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".to_string());

        div()
            .id(SharedString::from(format!("session-card-{}", session.id)))
            .flex()
            .flex_col()
            .gap_1()
            .mx_2()
            .my_0p5()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .hover(|style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
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
            })
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(title_color)
                            .truncate()
                            .child(session.title.clone()),
                    )
                    .children(status_indicator.map(|(color, icon)| {
                        div().flex_none().text_sm().text_color(color).child(icon)
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .child(IconName::Folder)
                            .child(div().truncate().child(project)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    ),
            )
            .context_menu(move |menu, _window, _cx| {
                let settle_model = context_model.clone();
                let settle_work_dir = context_work_dir.clone();
                let settle_session_id = context_session_id.clone();
                let remove_model = context_model.clone();
                let remove_work_dir = context_work_dir.clone();
                let remove_session_id = context_session_id.clone();

                menu.item(PopupMenuItem::new("Settle Session").on_click(
                    move |_event, _window, cx| {
                        settle_model.update(cx, |state, _cx| {
                            controller::dispatch(
                                state,
                                AppAction::SettleSession {
                                    work_dir: settle_work_dir.clone(),
                                    session_id: settle_session_id.clone(),
                                },
                            );
                        });
                    },
                ))
                .separator()
                .item(
                    PopupMenuItem::new("Remove Session").on_click(move |_event, _window, cx| {
                        remove_model.update(cx, |state, _cx| {
                            controller::dispatch(
                                state,
                                AppAction::RemoveSession {
                                    work_dir: remove_work_dir.clone(),
                                    session_id: remove_session_id.clone(),
                                },
                            );
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
                .icon(IconName::Settings)
                .tooltip("Settings")
                .ghost()
                .text_color(theme.muted_foreground)
                .on_click(move |_event, _window, cx| {
                    model.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::ToggleSettings);
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
        let mut sessions: Vec<SessionInfo> = state
            .projects
            .iter()
            .flat_map(|project| project.sessions.iter().cloned())
            .filter(|session| {
                query.is_empty()
                    || session.title.to_lowercase().contains(&query)
                    || session.id.to_lowercase().contains(&query)
            })
            .collect();
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.title.cmp(&right.title))
        });

        let mut grouped: Vec<(DateGroup, Vec<SessionInfo>)> = vec![
            (DateGroup::Today, Vec::new()),
            (DateGroup::Yesterday, Vec::new()),
            (DateGroup::ThisWeek, Vec::new()),
            (DateGroup::Older, Vec::new()),
        ];
        for session in sessions {
            let group = get_date_group(session.updated_at);
            if let Some((_, entries)) = grouped
                .iter_mut()
                .find(|(candidate, _)| *candidate == group)
            {
                entries.push(session);
            }
        }

        let mut children = Vec::new();
        for (group, sessions) in grouped {
            if sessions.is_empty() {
                continue;
            }
            if group != DateGroup::Today {
                children.push(
                    div()
                        .px_3()
                        .pt_3()
                        .pb_1()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(group.label())
                        .into_any_element(),
                );
            }
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
            .w(px(260.0))
            .h_full()
            .min_h_0()
            .bg(theme.title_bar)
            .border_r_1()
            .border_color(theme.title_bar_border)
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
