use gpui::InteractiveElement;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use std::path::PathBuf;

use crate::state::{AppState, ProjectInfo, SessionHealth, SessionInfo};

pub struct SidebarView {
    pub model: Entity<AppState>,
    pub search_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter sessions...")
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
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(rgb(0x2d2d30))
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
                            .text_color(rgb(0xa1a1aa))
                            .child("PROJECTS"),
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
                    .px_2()
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
            .border_color(rgb(0x2d2d30))
            .child(Input::new(&self.search_input))
    }

    fn render_session_row(
        &self,
        session: &SessionInfo,
        is_active: bool,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (dot_color, status_text) = match session.health {
            SessionHealth::Working => (rgb(0x3b82f6), "●"),
            SessionHealth::Healthy => (rgb(0x22c55e), "•"),
            SessionHealth::Warning => (rgb(0xeab308), "▲"),
        };

        let bg_color = if is_active {
            rgb(0x27272a)
        } else {
            rgb(0x18181b)
        };

        let text_color = if is_active {
            rgb(0xffffff)
        } else {
            rgb(0xa1a1aa)
        };

        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        let model = self.model.clone();

        div()
            .id(SharedString::from(format!("session-{}", session.id)))
            .flex()
            .items_center()
            .justify_between()
            .pl_6()
            .pr_3()
            .py_1p5()
            .rounded_md()
            .bg(bg_color)
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
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(dot_color)
                            .child(status_text),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(text_color)
                            .truncate()
                            .child(session.title.clone()),
                    ),
            )
    }

    fn render_project_group(
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
            .id(SharedString::from(format!("project-header-{}", project.name)))
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_1p5()
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if is_active_project {
                                rgb(0x38bdf8)
                            } else {
                                rgb(0xe4e4e7)
                            })
                            .child(project.name.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x71717a))
                    .child(format!("{}", project.sessions.len())),
            );

        let mut children = vec![header.into_any_element()];

        if project.is_expanded {
            for session in &project.sessions {
                if !search_query.is_empty()
                    && !session.title.to_lowercase().contains(&search_query)
                    && !session.id.to_lowercase().contains(&search_query)
                {
                    continue;
                }
                let is_active = is_active_project
                    && active_session_id.as_deref() == Some(&session.id);
                children.push(self.render_session_row(session, is_active, cx).into_any_element());
            }
        }

        div().flex().flex_col().children(children)
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
            .bg(rgb(0x18181b))
            .border_r_1()
            .border_color(rgb(0x2d2d30))
            .child(self.render_header(cx))
            .child(self.render_search(cx))
            .child(
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .py_2()
                    .children(projects.iter().map(|p| self.render_project_group(p, cx))),
            )
    }
}
