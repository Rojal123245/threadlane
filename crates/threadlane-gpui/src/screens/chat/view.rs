use std::collections::HashMap;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Disableable, Icon, IconName, Sizable};

use crate::app::{actions::AppAction, controller};
use crate::state::{AppState, ChatMessageInfo, MessageRole, ToolActivityInfo};

pub struct ChatListView {
    pub model: Entity<AppState>,
    pub input_state: Entity<TextareaState>,
    pub header_left_padding: Pixels,
    scroll_handle: ScrollHandle,
    markdown_states: HashMap<String, (String, Entity<TextViewState>)>,
    _subscriptions: Vec<Subscription>,
}

impl ChatListView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scroll_handle = ScrollHandle::new();
        let input_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Do anything...")
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .soft_wrap(true)
        });

        let sub1 = cx.observe(&model, |_this, _model, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let submit_scroll_handle = scroll_handle.clone();
        let sub2 = cx.subscribe_in(
            &input_state,
            window,
            move |_this, input_state, event: &InputEvent, window, cx| {
                cx.notify();
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    let text = input_state.read(cx).value().to_string();
                    if !text.trim().is_empty() {
                        let text_to_send = text.clone();
                        let is_generating = model_clone.read(cx).is_generating;
                        model_clone.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                if is_generating {
                                    AppAction::StageBusyMessage(text_to_send)
                                } else {
                                    AppAction::SendPrompt(text_to_send)
                                },
                            );
                            cx.notify();
                        });
                        input_state.update(cx, |state, cx| {
                            state.set_value("", window, cx);
                        });
                        submit_scroll_handle.scroll_to_bottom();
                    }
                }
            },
        );

        let stream_model = model.clone();
        let stream_scroll_handle = scroll_handle.clone();
        cx.spawn(async move |_this, cx| {
            let mut follow_tail = true;
            let mut settle_frames = 0_u8;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let has_event =
                    stream_model.read_with(cx, |state, _cx| state.chat_stream_pending());
                let changed = has_event
                    && stream_model.update(cx, |state, cx| {
                        let changed = state.drain_chat_stream();
                        if changed {
                            cx.notify();
                        }
                        changed
                    });

                if changed {
                    settle_frames = 4;
                } else if settle_frames == 0 {
                    let children = stream_scroll_handle.children_count();
                    follow_tail = children == 0
                        || stream_scroll_handle.bottom_item().saturating_add(1) >= children;
                }

                if follow_tail && (changed || settle_frames > 0) {
                    // Markdown parsing and text shaping can change the final
                    // row height after the event frame. Keep applying the tail
                    // anchor until a quiet frame observes an intentional
                    // scroll away from the latest row.
                    stream_scroll_handle.scroll_to_bottom();
                    stream_model.update(cx, |_state, cx| cx.notify());
                    if !changed {
                        settle_frames = settle_frames.saturating_sub(1);
                    }
                }
            }
        })
        .detach();

        Self {
            model,
            input_state,
            header_left_padding: px(14.0),
            scroll_handle,
            markdown_states: HashMap::new(),
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
            .w_full()
            .min_w_0()
            .overflow_hidden()
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
                    .w_full()
                    .min_w_0()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(activity.detail.clone()),
            )
    }

    fn render_message(&mut self, msg: &ChatMessageInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        match msg.role {
            MessageRole::User => div()
                .w_full()
                .min_w_0()
                .flex()
                .justify_end()
                .my_2()
                .px_4()
                .child(
                    div()
                        .min_w_0()
                        .max_w(px(600.0))
                        .p_3()
                        .rounded_lg()
                        .bg(theme.secondary)
                        .text_sm()
                        .text_color(theme.secondary_foreground)
                        .child({
                            let entry =
                                self.markdown_states
                                    .entry(msg.id.clone())
                                    .or_insert_with(|| {
                                        let content = msg.content.clone();
                                        let state =
                                            cx.new(|cx| TextViewState::markdown(&content, cx));
                                        (content, state)
                                    });
                            if entry.0 != msg.content {
                                entry.0 = msg.content.clone();
                                entry.1.update(cx, |state, cx| {
                                    state.set_text(&msg.content, cx);
                                });
                            }
                            TextView::new(&entry.1).selectable(true)
                        }),
                ),
            MessageRole::Assistant => {
                let tool_elements: Vec<_> = msg
                    .tool_activities
                    .iter()
                    .map(|tool| self.render_tool_activity(tool, cx))
                    .collect();

                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .my_2()
                    .px_4()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .max_w(px(720.0))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(if !msg.content.is_empty() {
                                Some(
                                    div()
                                        .w_full()
                                        .text_sm()
                                        .text_color(theme.foreground)
                                        .child({
                                            let entry = self
                                                .markdown_states
                                                .entry(msg.id.clone())
                                                .or_insert_with(|| {
                                                    let content = msg.content.clone();
                                                    let state = cx.new(|cx| {
                                                        TextViewState::markdown(&content, cx)
                                                    });
                                                    (content, state)
                                                });
                                            if entry.0 != msg.content {
                                                entry.0 = msg.content.clone();
                                                entry.1.update(cx, |state, cx| {
                                                    state.set_text(&msg.content, cx);
                                                });
                                            }
                                            TextView::new(&entry.1).selectable(true)
                                        })
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
        .into_any_element()
    }

    fn render_new_task(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let (projects, active_work_dir) = {
            let state = self.model.read(cx);
            (
                state
                    .projects
                    .iter()
                    .map(|project| (project.name.clone(), project.work_dir.clone()))
                    .collect::<Vec<_>>(),
                state.active_work_dir.clone(),
            )
        };
        let selected_project = projects
            .iter()
            .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "Choose a project".to_string());
        let model = self.model.clone();

        let project_picker = Button::new("new-task-project-picker")
            .icon(IconName::Folder)
            .label(selected_project)
            .dropdown_caret(true)
            .ghost()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for (name, work_dir) in projects.clone() {
                    let model = model.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(
                        move |_event, _window, cx| {
                            model.update(cx, |state, _cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectDraftProject(work_dir.clone()),
                                );
                            });
                        },
                    ));
                }

                let model = model.clone();
                menu.separator()
                    .item(PopupMenuItem::new("New project...").on_click(
                        move |_event, _window, cx| {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                model.update(cx, |state, _cx| {
                                    controller::dispatch(state, AppAction::AttachProject(path));
                                });
                            }
                        },
                    ))
            });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .pb(px(64.0))
            .child(
                div()
                    .text_2xl()
                    .text_color(theme.primary)
                    .child(IconName::Asterisk),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("What should we build in")
                    .child(project_picker)
                    .child("?"),
            )
            .into_any_element()
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let input_state = self.input_state.clone();
        let (selected_model, project_name, is_generating, session_status, pending_message) = {
            let state = self.model.read(cx);
            let project_name = state
                .active_work_dir
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "No project".to_string());
            (
                state.selected_model.clone(),
                project_name,
                state.is_generating,
                state.session_status.clone(),
                state.active_pending_composer_message().map(str::to_owned),
            )
        };
        let has_prompt = !self.input_state.read(cx).value().trim().is_empty();
        let project_root = self.model.read(cx).active_work_dir.clone();
        let model_options =
            crate::model_catalog::available_models_for_project(project_root.as_deref());
        let has_models = !model_options.is_empty();
        let selected_option = crate::model_catalog::available_option_for_project(
            &selected_model,
            project_root.as_deref(),
        );
        let model_label = selected_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "Connect a provider".to_string());
        let model_for_picker = self.model.clone();
        let queue_model = self.model.clone();
        let steer_model = self.model.clone();
        let dismiss_model = self.model.clone();
        let dismiss_input = self.input_state.clone();
        let stage_model = self.model.clone();
        let stage_input = self.input_state.clone();

        let pending_preview = pending_message.map(|text| {
            div()
                .w_full()
                .max_w(px(1000.0))
                .mx_auto()
                .mb_2()
                .h(px(52.0))
                .px_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(theme.secondary)
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child("Pending"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(theme.foreground)
                        .child(text),
                )
                .child(
                    Button::new("queue-pending-message")
                        .icon(IconName::Plus)
                        .xsmall()
                        .secondary()
                        .tooltip("Queue after the current response")
                        .on_click(move |_event, _window, cx| {
                            queue_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::QueuePendingMessage);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("steer-pending-message")
                        .icon(IconName::ArrowRight)
                        .xsmall()
                        .primary()
                        .tooltip("Steer the current response")
                        .on_click(move |_event, _window, cx| {
                            steer_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::SteerPendingMessage);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("dismiss-pending-message")
                        .icon(IconName::Undo2)
                        .xsmall()
                        .ghost()
                        .tooltip("Edit message in the composer")
                        .on_click(move |_event, window, cx| {
                            let restored = dismiss_model.update(cx, |state, cx| {
                                let restored =
                                    state.active_pending_composer_message().map(str::to_owned);
                                controller::dispatch(state, AppAction::DismissPendingMessage);
                                cx.notify();
                                restored
                            });
                            if let Some(restored) = restored {
                                dismiss_input.update(cx, |input, cx| {
                                    input.set_value(restored, window, cx);
                                });
                            }
                        }),
                )
        });

        let model_picker = Button::new("composer-model-picker")
            .label(model_label)
            .dropdown_caret(true)
            .ghost()
            .disabled(!has_models);
        let model_picker = if let Some(option) = selected_option.as_ref() {
            model_picker.icon(Icon::default().path(option.provider.icon_path()))
        } else {
            model_picker
        };
        let model_picker = model_picker.dropdown_menu(move |menu, _window, _cx| {
            model_options.iter().cloned().fold(menu, |menu, option| {
                let model = model_for_picker.clone();
                menu.item(
                    PopupMenuItem::new(option.label)
                        .icon(Icon::default().path(option.provider.icon_path()))
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, _cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectModel(option.id.to_string()),
                                );
                            });
                        }),
                )
            })
        });

        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb_2()
            .bg(theme.background)
            .children(pending_preview)
            .child(
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .min_h(px(132.0))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .child(
                        Textarea::new(&self.input_state)
                            .appearance(false)
                            .bordered(false),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(model_picker)
                            .child(div().flex_1())
                            .children(is_generating.then(|| {
                                Button::new("stage-message-btn")
                                    .w(px(40.0))
                                    .h(px(40.0))
                                    .label("↑")
                                    .tooltip("Choose Queue or Steer")
                                    .primary()
                                    .disabled(!has_prompt)
                                    .on_click(move |_event, window, cx| {
                                        let text = stage_input.read(cx).value().to_string();
                                        if text.trim().is_empty() {
                                            return;
                                        }
                                        stage_model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::StageBusyMessage(text),
                                            );
                                            cx.notify();
                                        });
                                        stage_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx);
                                        });
                                    })
                            }))
                            .child(
                                Button::new("send-btn")
                                    .w(px(40.0))
                                    .h(px(40.0))
                                    .label(if is_generating { "■" } else { "↑" })
                                    .tooltip(if is_generating {
                                        "Stop generation"
                                    } else {
                                        "Send message"
                                    })
                                    .when(is_generating, |button| button.danger())
                                    .when(!is_generating, |button| button.primary())
                                    .disabled(!is_generating && !has_prompt)
                                    .on_click(move |_event, window, cx| {
                                        if is_generating {
                                            model.update(cx, |state, cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::CancelGeneration,
                                                );
                                                cx.notify();
                                            });
                                            return;
                                        }
                                        let text = input_state.read(cx).value().to_string();
                                        if !text.trim().is_empty() {
                                            let text_to_send = text.clone();
                                            model.update(cx, |state, cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::SendPrompt(text_to_send),
                                                );
                                                cx.notify();
                                            });
                                            input_state.update(cx, |state, cx| {
                                                state.set_value("", window, cx);
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(IconName::Folder)
                    .child(project_name)
                    .child("·")
                    .child("Local")
                    .children(session_status.map(|status| {
                        div().flex().items_center().gap_2().child("·").child(status)
                    })),
            )
    }
}

impl Render for ChatListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (messages, is_new_task) = {
            let state = self.model.read(cx);
            (state.messages.clone(), state.is_new_task)
        };
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(if is_new_task {
                self.render_new_task(cx)
            } else if messages.is_empty() {
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
                    .id("chat-transcript")
                    .w_full()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .track_scroll(&self.scroll_handle)
                    .overflow_y_scrollbar()
                    .pt_3()
                    .pb_6()
                    .children(messages.iter().map(|m| self.render_message(m, cx)))
                    .into_any_element()
            })
            .child(self.render_composer(cx))
    }
}
