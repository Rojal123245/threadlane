use gpui::InteractiveElement;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use std::path::PathBuf;

use crate::state::{AppState, ProjectInfo, SessionHealth, SessionInfo};

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
            Self::Today => "TODAY",
            Self::Yesterday => "YESTERDAY",
            Self::ThisWeek => "THIS WEEK",
            Self::Older => "OLDER",
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
        0..=59 => "just now".to_string(),
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
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter session history...")
        });

        let sub1 = cx.observe(&model, |_this, _model, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let sub2 = cx.subscribe_in(&search_input, window, move |_this, search_input, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                let query = search_input.read(cx).value().to_string();
                model_clone.update(cx, |state, _cx| {
                    state.search_query = query;
                });
            }
        });

        Self {
            model,
            search_input,
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let total_projects = state.projects.len();
        let model = self.model.clone();

        div()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(72.0)) // Traffic light offset for macOS
            .pr_3()
            .h(px(48.0))
            .border_b_1()
            .border_color(rgb(0x27272a))
            .bg(rgb(0x18181b))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(0xe4e4e7))
                            .child("THREADLANE"),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(rgb(0x27272a))
                            .text_xs()
                            .text_color(rgb(0x71717a))
                            .child(format!("{total_projects}")),
                    ),
            )
            .child(
                div()
                    .id("attach-project-btn")
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x27272a))
                    .hover(|style| style.bg(rgb(0x3f3f46)))
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(0xe4e4e7))
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            model.update(cx, |state, _cx| {
                                let _ = state.attach_project(path);
                            });
                        }
                    })
                    .child("+ Attach"),
            )
    }

    fn render_search(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(rgb(0x27272a))
            .child(Input::new(&self.search_input))
    }

    fn render_session_card(
        &self,
        session: &SessionInfo,
        is_active: bool,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (dot_color, status_icon) = match session.health {
            SessionHealth::Working => (rgb(0x3b82f6), "●"),
            SessionHealth::Healthy => (rgb(0x22c55e), "●"),
            SessionHealth::Warning => (rgb(0xeab308), "▲"),
        };

        let bg_color = if is_active {
            rgb(0x27272a)
        } else {
            rgb(0x18181b)
        };

        let border_color = if is_active {
            rgb(0x3b82f6)
        } else {
            rgb(0x18181b)
        };

        let title_color = if is_active {
            rgb(0xffffff)
        } else {
            rgb(0xe4e4e7)
        };

        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        let model = self.model.clone();
        let time_ago = format_time_ago(session.updated_at);

        div()
            .id(SharedString::from(format!("session-card-{}", session.id)))
            .flex()
            .items_center()
            .justify_between()
            .mx_2()
            .my_0p5()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .hover(|style| style.bg(rgb(0x27272a)))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                let work_dir = work_dir.clone();
                let session_id = session_id.clone();
                model.update(cx, |state, _cx| {
                    state.select_session(work_dir, session_id);
                });
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(dot_color)
                            .child(status_icon),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(if is_active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                                    .text_color(title_color)
                                    .truncate()
                                    .max_w(px(160.0))
                                    .child(session.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x71717a))
                                    .child(time_ago),
                            ),
                    ),
            )
    }

    fn render_project_section(
        &self,
        project: &ProjectInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.model.read(cx);
        let is_active_project = state
            .active_work_dir
            .as_ref()
            .map_or(false, |dir| dir == &project.work_dir);

        let active_session_id = state.active_session_id.clone();
        let search_query = state.search_query.trim().to_lowercase();
        let work_dir: PathBuf = project.work_dir.clone();
        let model = self.model.clone();

        let header = div()
            .id(SharedString::from(format!("project-sec-{}", project.name)))
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                let work_dir = work_dir.clone();
                model.update(cx, |state, _cx| {
                    state.toggle_project_expanded(&work_dir);
                });
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x71717a))
                            .child(if project.is_expanded { "▼" } else { "▶" }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(if is_active_project {
                                rgb(0x38bdf8)
                            } else {
                                rgb(0xa1a1aa)
                            })
                            .child(project.name.to_uppercase()),
                    ),
            )
            .child(
                div()
                    .px_1p5()
                    .py_0p5()
                    .rounded_full()
                    .bg(rgb(0x27272a))
                    .text_xs()
                    .text_color(rgb(0x71717a))
                    .child(format!("{}", project.sessions.len())),
            );

        let mut children = vec![header.into_any_element()];

        if project.is_expanded {
            // Group sessions by DateGroup (Today, Yesterday, ThisWeek, Older)
            let mut groups: Vec<(DateGroup, Vec<&SessionInfo>)> = vec![
                (DateGroup::Today, Vec::new()),
                (DateGroup::Yesterday, Vec::new()),
                (DateGroup::ThisWeek, Vec::new()),
                (DateGroup::Older, Vec::new()),
            ];

            for session in &project.sessions {
                if !search_query.is_empty()
                    && !session.title.to_lowercase().contains(&search_query)
                    && !session.id.to_lowercase().contains(&search_query)
                {
                    continue;
                }
                let group_type = get_date_group(session.updated_at);
                if let Some((_, list)) = groups.iter_mut().find(|(g, _)| *g == group_type) {
                    list.push(session);
                }
            }

            for (group, sessions) in groups {
                if sessions.is_empty() {
                    continue;
                }
                children.push(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(0x52525b))
                        .child(group.label())
                        .into_any_element(),
                );
                for session in sessions {
                    let is_active = is_active_project
                        && active_session_id.as_deref() == Some(&session.id);
                    children.push(self.render_session_card(session, is_active, cx).into_any_element());
                }
            }
        }

        div().flex().flex_col().mb_2().children(children)
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let projects = self.model.read(cx).projects.clone();

        div()
            .flex()
            .flex_col()
            .w(px(260.0))
            .h_full()
            .min_h_0()
            .bg(rgb(0x18181b))
            .border_r_1()
            .border_color(rgb(0x27272a))
            .child(self.render_header(cx))
            .child(self.render_search(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .py_2()
                    .children(projects.iter().map(|p| self.render_project_section(p, cx))),
            )
    }
}
