use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::theme::ActiveTheme;

use crate::app::{actions::AppAction, controller};
use crate::state::{AppState, ChatMessageInfo, MessageRole, ToolActivityInfo};

pub struct ChatListView {
    pub model: Entity<AppState>,
    pub input_state: Entity<InputState>,
    pub header_left_padding: Pixels,
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
        let sub2 = cx.subscribe_in(
            &input_state,
            window,
            move |_this, input_state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let text = input_state.read(cx).value().to_string();
                    if !text.trim().is_empty() {
                        let text_to_send = text.clone();
                        model_clone.update(cx, |state, _cx| {
                            controller::dispatch(state, AppAction::SendPrompt(text_to_send));
                        });
                        input_state.update(cx, |state, cx| {
                            state.set_value("", window, cx);
                        });
                    }
                }
            },
        );

        Self {
            model,
            input_state,
            header_left_padding: px(14.0),
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_title = {
            let state = self.model.read(cx);
            state
                .projects
                .iter()
                .flat_map(|project| project.sessions.iter())
                .find(|session| state.active_session_id.as_deref() == Some(&session.id))
                .map(|session| session.title.clone())
                .unwrap_or_else(|| "New task".to_string())
        };
        let theme = cx.theme().colors;

        div()
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_start()
            .pt(px(9.0))
            .pl(self.header_left_padding)
            .pr_4()
            .border_b_1()
            .border_color(theme.title_bar_border)
            .bg(theme.title_bar)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(active_title),
            )
    }

    fn render_tool_activity(
        &self,
        activity: &ToolActivityInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (badge_bg, badge_fg) = match activity.category.as_str() {
            "Created" | "Edited" => (theme.success, theme.success_foreground),
            "Ran" => (theme.info, theme.info_foreground),
            "Error" => (theme.danger, theme.danger_foreground),
            _ => (theme.muted, theme.muted_foreground),
        };

        div()
            .flex()
            .flex_col()
            .my_1()
            .p_2()
            .rounded_md()
            .bg(theme.title_bar)
            .border_1()
            .border_color(theme.border)
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
                            .text_color(theme.foreground)
                            .child(activity.title.clone()),
                    ),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(activity.detail.clone()),
            )
    }

    fn render_message(&self, msg: &ChatMessageInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        match msg.role {
            MessageRole::User => div().flex().justify_end().my_2().px_4().child(
                div()
                    .max_w(px(600.0))
                    .p_3()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .text_sm()
                    .text_color(theme.secondary_foreground)
                    .child(msg.content.clone()),
            ),
            MessageRole::Assistant => {
                let tool_elements: Vec<_> = msg
                    .tool_activities
                    .iter()
                    .map(|tool| self.render_tool_activity(tool, cx))
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
                                    .text_color(theme.foreground)
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
                    .text_color(theme.muted_foreground)
                    .child(msg.content.clone()),
            ),
            MessageRole::Error => div().flex().justify_center().my_2().px_4().child(
                div()
                    .max_w(px(720.0))
                    .w_full()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.danger)
                    .border_1()
                    .border_color(theme.danger)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.danger_foreground)
                                    .child("ERROR"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.danger_foreground)
                                    .child(msg.content.clone()),
                            ),
                    ),
            ),
        }
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let input_state = self.input_state.clone();

        div()
            .p_3()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Input::new(&self.input_state)
                                .appearance(false)
                                .bordered(false),
                        ),
                    )
                    .child(Button::new("send-btn").label("Send").primary().on_click(
                        move |_event, window, cx| {
                            let text = input_state.read(cx).value().to_string();
                            if !text.trim().is_empty() {
                                let text_to_send = text.clone();
                                model.update(cx, |state, _cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SendPrompt(text_to_send),
                                    );
                                });
                                input_state.update(cx, |state, cx| {
                                    state.set_value("", window, cx);
                                });
                            }
                        },
                    )),
            )
    }
}

impl Render for ChatListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let messages = self.model.read(cx).messages.clone();
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_h_0()
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(if messages.is_empty() {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div().text_sm().text_color(theme.muted_foreground).child(
                            "No messages in this session yet. Type a prompt below to begin.",
                        ),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .py_3()
                    .children(messages.iter().map(|m| self.render_message(m, cx)))
                    .into_any_element()
            })
            .child(self.render_composer(cx))
    }
}
