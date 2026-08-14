use gpui::InteractiveElement;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;

use crate::state::{AppState, ChatMessageInfo, MessageRole, ToolActivityInfo};

pub struct ChatListView {
    pub model: Entity<AppState>,
    pub input_state: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ChatListView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Ask Threadlane...")
                .clean_on_escape()
        });

        let sub1 = cx.observe(&model, |_this, _model, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let sub2 = cx.subscribe_in(&input_state, window, move |_this, input_state, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let text = input_state.read(cx).value().to_string();
                if !text.trim().is_empty() {
                    let text_to_send = text.clone();
                    model_clone.update(cx, |state, _cx| {
                        let _ = state.send_prompt(text_to_send);
                    });
                    input_state.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                }
            }
        });

        Self {
            model,
            input_state,
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let active_title = state
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| state.active_session_id.as_deref() == Some(&s.id))
            .map(|s| s.title.as_str())
            .unwrap_or("No active session");

        let model = self.model.clone();

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(0x2d2d30))
            .bg(rgb(0x18181b))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0xf4f4f5))
                            .child(active_title.to_string()),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(rgb(0x1e3a8a))
                            .text_xs()
                            .text_color(rgb(0x60a5fa))
                            .child(if state.active_session_id.is_some() {
                                "Active"
                            } else {
                                "Idle"
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
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
                                model.update(cx, |state, _cx| {
                                    let _ = state.create_new_session();
                                });
                            })
                            .child("+ New Session"),
                    )
                    .child(
                        div()
                            .px_2p5()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x27272a))
                            .text_xs()
                            .text_color(rgb(0xa1a1aa))
                            .child("Claude 3.5 Sonnet"),
                    ),
            )
    }

    fn render_tool_activity(&self, activity: &ToolActivityInfo) -> impl IntoElement {
        let badge_bg = match activity.category.as_str() {
            "Created" | "Edited" => rgb(0x064e3b),
            "Ran" => rgb(0x312e81),
            "Error" => rgb(0x7f1d1d),
            _ => rgb(0x27272a),
        };

        let badge_fg = match activity.category.as_str() {
            "Created" | "Edited" => rgb(0x34d399),
            "Ran" => rgb(0x818cf8),
            "Error" => rgb(0xfca5a5),
            _ => rgb(0xa1a1aa),
        };

        div()
            .flex()
            .flex_col()
            .my_1()
            .p_2()
            .rounded_md()
            .bg(rgb(0x18181b))
            .border_1()
            .border_color(rgb(0x2d2d30))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(badge_bg)
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(badge_fg)
                            .child(activity.category.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(0xe4e4e7))
                            .child(activity.title.clone()),
                    ),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(rgb(0x71717a))
                    .child(activity.detail.clone()),
            )
    }

    fn render_message(&self, msg: &ChatMessageInfo, _cx: &mut Context<Self>) -> impl IntoElement {
        match msg.role {
            MessageRole::User => div()
                .flex()
                .justify_end()
                .my_2()
                .px_4()
                .child(
                    div()
                        .max_w(px(600.0))
                        .p_3()
                        .rounded_lg()
                        .bg(rgb(0x27272a))
                        .text_sm()
                        .text_color(rgb(0xffffff))
                        .child(msg.content.clone()),
                ),
            MessageRole::Assistant => {
                let tool_elements: Vec<_> = msg
                    .tool_activities
                    .iter()
                    .map(|tool| self.render_tool_activity(tool))
                    .collect();

                div().flex().flex_col().my_2().px_4().child(
                    div()
                        .max_w(px(720.0))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(if !msg.content.is_empty() {
                            Some(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0xe4e4e7))
                                    .child(msg.content.clone())
                                    .into_any_element(),
                            )
                        } else {
                            None
                        })
                        .children(tool_elements),
                )
            }
            MessageRole::System => div().flex().justify_center().my_2().child(
                div()
                    .text_xs()
                    .text_color(rgb(0x71717a))
                    .child(msg.content.clone()),
            ),
        }
    }

    fn render_composer(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let input_state = self.input_state.clone();

        div()
            .p_3()
            .border_t_1()
            .border_color(rgb(0x2d2d30))
            .bg(rgb(0x18181b))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.input_state)))
                    .child(
                        div()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .bg(rgb(0x3b82f6))
                            .hover(|style| style.bg(rgb(0x2563eb)))
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(0xffffff))
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let text = input_state.read(cx).value().to_string();
                                if !text.trim().is_empty() {
                                    let text_to_send = text.clone();
                                    model.update(cx, |state, _cx| {
                                        let _ = state.send_prompt(text_to_send);
                                    });
                                    input_state.update(cx, |state, cx| {
                                        state.set_value("", window, cx);
                                    });
                                }
                            })
                            .child("Send"),
                    ),
            )
    }
}

impl Render for ChatListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let messages = self.model.read(cx).messages.clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .bg(rgb(0x09090b))
            .child(self.render_header(cx))
            .child(if messages.is_empty() {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x71717a))
                            .child("No messages in this session yet. Type a prompt below to begin."),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .py_3()
                    .children(messages.iter().map(|m| self.render_message(m, cx)))
                    .into_any_element()
            })
            .child(self.render_composer(cx))
    }
}
