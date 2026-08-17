use std::collections::{HashMap, HashSet};

use std::time::Duration;

use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::theme::ActiveTheme;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable, Icon, IconName, Sizable};

use crate::app::{actions::AppAction, controller};
use crate::state::{AppState, ChatMessageInfo, MessageRole, ToolActivityInfo};
use threadlane_agent::{ImageAttachment, PlanItemStatus, ReasoningEffort, SessionPlan};
use threadlane_coding_agent::commands::available_slash_commands;

actions!(threadlane_composer, [PasteClipboard]);

const INPUT_KEY_CONTEXT: &str = "Input";

pub fn init(cx: &mut App) {
    // gpui-component's Textarea owns the focused `Input` context. Register
    // after gpui-component initialization so this action can inspect image
    // clipboard entries while preserving text paste behavior.
    cx.bind_keys([
        KeyBinding::new("cmd-v", PasteClipboard, Some(INPUT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-v", PasteClipboard, Some(INPUT_KEY_CONTEXT)),
    ]);
}

pub struct ChatListView {
    pub model: Entity<AppState>,
    pub input_state: Entity<TextareaState>,
    pub header_left_padding: Pixels,
    scroll_handle: ScrollHandle,
    expanded_activity_groups: HashSet<String>,
    markdown_states: HashMap<String, (String, Entity<TextViewState>)>,
    pasted_images: Vec<ImageAttachment>,
    branches: Vec<String>,
    current_checkout: Option<String>,
    branch_error: Option<String>,
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
            move |this, input_state, event: &InputEvent, window, cx| {
                cx.notify();
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    let text = input_state.read(cx).value().to_string();
                    let is_generating = model_clone.read(cx).is_generating;
                    if !text.trim().is_empty() || (!is_generating && !this.pasted_images.is_empty())
                    {
                        let images = if is_generating {
                            Vec::new()
                        } else {
                            std::mem::take(&mut this.pasted_images)
                        };
                        model_clone.update(cx, |state, cx| {
                            if is_generating {
                                controller::dispatch(state, AppAction::StageBusyMessage(text));
                                controller::dispatch(state, AppAction::QueuePendingMessage);
                            } else {
                                controller::dispatch(
                                    state,
                                    AppAction::SendPromptWithImages { text, images },
                                );
                            }
                            cx.notify();
                        });
                        input_state.update(cx, |state, cx| {
                            state.set_value("", window, cx);
                        });
                        submit_scroll_handle.scroll_to_bottom();
                        cx.notify();
                    }
                }
            },
        );

        let stream_model = model.clone();
        let stream_scroll_handle = scroll_handle.clone();
        cx.spawn(async move |this, cx| {
            let mut follow_tail = true;
            let mut settle_frames = 0_u8;
            loop {
                // Event-driven pacing: check quickly when generating,
                // slow down when idle.
                let interval = if settle_frames > 0 {
                    Duration::from_millis(16) // ~60fps for animation frames
                } else {
                    Duration::from_millis(33)
                };
                cx.background_executor().timer(interval).await;

                // Re-arm tail following as soon as the user manually returns to the bottom,
                // even while stream events keep arriving and settling never reaches zero.
                if !follow_tail {
                    let distance_from_bottom = (stream_scroll_handle.offset().y
                        + stream_scroll_handle.max_offset().y)
                        .abs();
                    follow_tail = distance_from_bottom <= px(24.0);
                }

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
                    // Markdown measurement and new rows can complete after the first redraw.
                    settle_frames = 6;
                } else if settle_frames == 0 {
                    let distance_from_bottom = (stream_scroll_handle.offset().y
                        + stream_scroll_handle.max_offset().y)
                        .abs();
                    follow_tail = distance_from_bottom <= px(24.0);
                }

                if follow_tail && (changed || settle_frames > 0) {
                    stream_scroll_handle.scroll_to_bottom();
                    let _ = this.update(cx, |_this, cx| cx.notify());
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
            expanded_activity_groups: HashSet::new(),
            markdown_states: HashMap::new(),
            pasted_images: Vec::new(),
            branches: Vec::new(),
            current_checkout: None,
            branch_error: None,
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn refresh_branches(&self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    threadlane_git::inspect(&work_dir)
                        .map_err(|error| format!("{}: {}", error.work_dir.display(), error.message))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(status) => {
                        this.current_checkout = status.branch;
                        this.branches = status.branches;
                        this.branch_error = None;
                    }
                    Err(error) => {
                        this.branch_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn checkout_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { threadlane_git::checkout(&work_dir, &branch) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if result.is_ok() {
                    this.refresh_branches(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn paste_composer_clipboard(
        &mut self,
        _action: &PasteClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = clipboard.text().filter(|text| !text.is_empty()) {
            self.input_state.update(cx, |input, cx| {
                input.insert(text, window, cx);
            });
        }

        let mut pasted = 0;
        for entry in clipboard.entries {
            let ClipboardEntry::Image(image) = entry else {
                continue;
            };
            if image.bytes.is_empty() {
                continue;
            }

            let mime_type = image.format.mime_type();
            let extension = mime_type.strip_prefix("image/").unwrap_or("png");
            self.pasted_images.push(ImageAttachment {
                display_name: format!("Pasted image {}.{extension}", self.pasted_images.len() + 1),
                data_url: format!(
                    "data:{mime_type};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(image.bytes)
                ),
            });
            pasted += 1;
        }

        cx.stop_propagation();
        if pasted > 0 {
            cx.notify();
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

    fn render_plan_tracker(
        &self,
        plan: &SessionPlan,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if plan.items.is_empty() {
            return None;
        }

        let theme = cx.theme().colors;
        let completed = plan
            .items
            .iter()
            .filter(|item| item.status == PlanItemStatus::Completed)
            .count();
        let total = plan.items.len();
        let progress_width = 72.0 * completed as f32 / total as f32;
        let tooltip_plan = plan.clone();

        Some(
            div()
                .id("session-plan-hover-region")
                .w_full()
                .flex_none()
                .flex()
                .justify_center()
                .py_1()
                .tooltip(move |window, cx| {
                    let colors = cx.theme().colors;
                    let content_plan = tooltip_plan.clone();
                    Tooltip::element(move |_window, _cx| {
                        let rows = content_plan.items.iter().enumerate().map(|(index, item)| {
                            let (marker, color) = match item.status {
                                PlanItemStatus::Completed => ("✓", colors.success),
                                PlanItemStatus::InProgress => ("●", colors.primary),
                                PlanItemStatus::Pending => ("○", colors.muted_foreground),
                            };
                            div()
                                .flex()
                                .items_start()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(16.0))
                                        .flex_none()
                                        .text_center()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(color)
                                        .child(marker),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .text_sm()
                                        .text_color(colors.foreground)
                                        .child(format!("{}. {}", index + 1, item.step)),
                                )
                        });
                        div()
                            .w(px(640.0))
                            .max_h(px(280.0))
                            .overflow_y_scrollbar()
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(content_plan.explanation.clone().map(|explanation| {
                                div()
                                    .pb_2()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .text_sm()
                                    .text_color(colors.muted_foreground)
                                    .child(explanation)
                            }))
                            .children(rows)
                    })
                    .build(window, cx)
                })
                .child(
                    Button::new("session-plan-tracker").ghost().child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{completed}/{total}")),
                            )
                            .child(
                                div()
                                    .w(px(72.0))
                                    .h(px(3.0))
                                    .rounded_full()
                                    .overflow_hidden()
                                    .bg(theme.border)
                                    .child(
                                        div()
                                            .w(px(progress_width))
                                            .h_full()
                                            .rounded_full()
                                            .bg(theme.primary),
                                    ),
                            ),
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_tool_activity(
        &self,
        activity: &ToolActivityInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (marker, marker_color, is_active) = match activity.category.as_str() {
            "Error" => ("!", theme.danger, false),
            "Working" | "Thinking" => ("◌", theme.primary, true),
            "Completed" | "Edited" | "Created" | "Ran" | "Loaded" => ("✓", theme.success, false),
            _ => ("✓", theme.muted_foreground, false),
        };
        let model = self.model.clone();
        let tool_call_id = activity.id.clone();
        let has_detail = !activity.detail.trim().is_empty();
        let row_id = SharedString::from(activity.id.clone());

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .py_1()
            .child(
                div()
                    .id(row_id)
                    .h(px(28.0))
                    .px_1()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(has_detail, |row| {
                        row.cursor_pointer()
                            .hover(|row| row.bg(theme.muted))
                            .on_click(move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::ToggleToolActivity(tool_call_id.clone()),
                                    );
                                    cx.notify();
                                });
                            })
                    })
                    .child({
                        let marker_el = div()
                            .w(px(18.0))
                            .flex_none()
                            .text_center()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(marker_color)
                            .child(marker);
                        if is_active {
                            marker_el
                                .with_animation(
                                    SharedString::from(format!("tool-pulse-{}", activity.id)),
                                    Animation::new(Duration::from_millis(1000))
                                        .repeat()
                                        .with_easing(ease_in_out),
                                    |el, delta| {
                                        el.opacity(
                                            0.3 + 0.7 * (delta * std::f32::consts::PI).sin().abs(),
                                        )
                                    },
                                )
                                .into_any_element()
                        } else {
                            marker_el.into_any_element()
                        }
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(activity.summary.clone()),
                    )
                    .children(has_detail.then(|| {
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if activity.is_expanded { "⌄" } else { "›" })
                    })),
            )
            .children(activity.is_expanded.then(|| {
                div()
                    .ml(px(26.0))
                    .mt_1()
                    .p_2()
                    .max_h(px(240.0))
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .overflow_y_scrollbar()
                    .child(activity.detail.clone())
            }))
    }

    fn render_activity_group(
        &mut self,
        activities: &[ToolActivityInfo],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const RECENT_ACTIVITY_LIMIT: usize = 4;

        let theme = cx.theme().colors;
        let group_id = activities
            .first()
            .map(|activity| activity.id.clone())
            .unwrap_or_else(|| "empty".into());
        let is_expanded = self.expanded_activity_groups.contains(&group_id);
        let hidden_count = activities.len().saturating_sub(RECENT_ACTIVITY_LIMIT);
        let visible_start = if is_expanded { 0 } else { hidden_count };
        let activity_rows = activities[visible_start..]
            .iter()
            .map(|activity| self.render_tool_activity(activity, cx))
            .collect::<Vec<_>>();
        let button_group_id = group_id.clone();

        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .my_1()
            .px_4()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(px(720.0))
                    .flex()
                    .flex_col()
                    .children((hidden_count > 0).then(|| {
                        Button::new(SharedString::from(format!("activity-group-{group_id}")))
                            .xsmall()
                            .ghost()
                            .justify_start()
                            .text_color(theme.muted_foreground)
                            .label(if is_expanded {
                                "Collapse earlier activities".to_string()
                            } else {
                                format!("{hidden_count} earlier activities")
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                if !this.expanded_activity_groups.remove(&button_group_id) {
                                    this.expanded_activity_groups
                                        .insert(button_group_id.clone());
                                }
                                cx.notify();
                            }))
                    }))
                    .children(activity_rows),
            )
            .into_any_element()
    }

    fn render_working_indicator(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .my_1()
            .px_4()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(px(720.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .with_animation(
                                SharedString::from("working-wave-dots"),
                                Animation::new(Duration::from_millis(1200))
                                    .repeat()
                                    .with_easing(ease_in_out),
                                |el, delta| {
                                    let opacity =
                                        0.35 + 0.65 * (delta * std::f32::consts::PI).sin().abs();
                                    el.opacity(opacity)
                                },
                            )
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Working…"),
                    ),
            )
            .into_any_element()
    }

    fn render_transcript_rows(
        &mut self,
        messages: &[ChatMessageInfo],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let mut index = 0;
        while index < messages.len() {
            let message = &messages[index];
            let is_activity_only = message.role == MessageRole::Assistant
                && message.content.is_empty()
                && message.reasoning_content.is_none()
                && message
                    .tool_activities
                    .iter()
                    .any(|activity| activity.title != "update_plan");
            if !is_activity_only {
                rows.push(self.render_message(message, cx));
                index += 1;
                continue;
            }

            let mut activities = Vec::new();
            while index < messages.len() {
                let candidate = &messages[index];
                let candidate_is_activity_only = candidate.role == MessageRole::Assistant
                    && candidate.content.is_empty()
                    && candidate.reasoning_content.is_none()
                    && candidate
                        .tool_activities
                        .iter()
                        .any(|activity| activity.title != "update_plan");
                if !candidate_is_activity_only {
                    break;
                }
                activities.extend(
                    candidate
                        .tool_activities
                        .iter()
                        .filter(|activity| activity.title != "update_plan")
                        .cloned(),
                );
                index += 1;
            }
            rows.push(self.render_activity_group(&activities, cx));
        }

        if self.model.read(cx).is_generating {
            rows.push(self.render_working_indicator(cx));
        }

        rows
    }

    fn render_reasoning_block(
        &mut self,
        msg: &ChatMessageInfo,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let reasoning = msg.reasoning_content.as_deref()?;
        if reasoning.trim().is_empty() {
            return None;
        }
        let theme = cx.theme().colors;
        let is_streaming = msg.streaming;
        let is_expanded = msg.reasoning_expanded;
        let model = self.model.clone();
        let msg_id = msg.id.clone();

        let icon_element = if is_streaming {
            div()
                .text_xs()
                .text_color(theme.primary)
                .with_animation(
                    SharedString::from(format!("reasoning-pulse-{}", msg.id)),
                    Animation::new(Duration::from_millis(1200))
                        .repeat()
                        .with_easing(ease_in_out),
                    |el, delta| {
                        let opacity = 0.4 + 0.6 * (delta * std::f32::consts::PI).sin().abs();
                        el.opacity(opacity)
                    },
                )
                .child("✦")
                .into_any_element()
        } else {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("✦")
                .into_any_element()
        };

        let header = div()
            .id(SharedString::from(format!("reasoning-toggle-{}", msg.id)))
            .h(px(28.0))
            .px_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .child(
                div()
                    .w(px(18.0))
                    .flex_none()
                    .text_center()
                    .child(icon_element),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if is_streaming {
                        "Thinking…"
                    } else {
                        "Thought process"
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if is_expanded { "⌄" } else { "›" }),
            )
            .on_click(move |_event, _window, cx| {
                model.update(cx, |state, cx| {
                    controller::dispatch(state, AppAction::ToggleReasoningExpanded(msg_id.clone()));
                    cx.notify();
                });
            });

        let detail = is_expanded.then(|| {
            let entry = self
                .markdown_states
                .entry(format!("reasoning-{}", msg.id))
                .or_insert_with(|| {
                    let state = cx.new(|cx| TextViewState::markdown(reasoning, cx));
                    (reasoning.to_string(), state)
                });
            if entry.0 != reasoning {
                entry.0 = reasoning.to_string();
                entry.1.update(cx, |state, cx| {
                    state.set_text(reasoning, cx);
                });
            }
            div()
                .ml(px(26.0))
                .mt_1()
                .p_2()
                .max_h(px(300.0))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_y_scrollbar()
                .child(TextView::new(&entry.1).selectable(true))
        });

        Some(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(header)
                .children(detail)
                .into_any_element(),
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
                        })
                        .context_menu({
                            let content = msg.content.clone();
                            move |menu, _window, _cx| {
                                let text = content.clone();
                                menu.item(PopupMenuItem::new("Copy Message").on_click(
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ));
                                    },
                                ))
                            }
                        }),
                ),
            MessageRole::Assistant => {
                let reasoning_element = self.render_reasoning_block(msg, cx);
                let tool_elements: Vec<_> = msg
                    .tool_activities
                    .iter()
                    .filter(|tool| tool.title != "update_plan")
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
                            .children(reasoning_element)
                            .children(if !msg.content.is_empty() {
                                let content_element = div()
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
                                    });

                                Some(if msg.streaming {
                                    content_element
                                        .with_animation(
                                            SharedString::from(format!("stream-text-{}", msg.id)),
                                            Animation::new(Duration::from_millis(150)),
                                            |el, delta| el.opacity(0.85 + 0.15 * delta),
                                        )
                                        .into_any_element()
                                } else {
                                    content_element.into_any_element()
                                })
                            } else {
                                None
                            })
                            .children(tool_elements)
                            .context_menu({
                                let content = msg.content.clone();
                                move |menu, _window, _cx| {
                                    let text = content.clone();
                                    menu.item(PopupMenuItem::new("Copy Message").on_click(
                                        move |_event, _window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                        },
                                    ))
                                }
                            }),
                    )
            }
            MessageRole::System => div().flex().justify_center().my_2().child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(msg.content.clone())
                    .context_menu({
                        let content = msg.content.clone();
                        move |menu, _window, _cx| {
                            let text = content.clone();
                            menu.item(PopupMenuItem::new("Copy Message").on_click(
                                move |_event, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                },
                            ))
                        }
                    }),
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
                                    .child(msg.content.clone())
                                    .context_menu({
                                        let content = msg.content.clone();
                                        move |menu, _window, _cx| {
                                            let text = content.clone();
                                            menu.item(PopupMenuItem::new("Copy Message").on_click(
                                                move |_event, _window, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(text.clone()),
                                                    );
                                                },
                                            ))
                                        }
                                    }),
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
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectDraftProject(work_dir.clone()),
                                );
                                cx.notify();
                            });
                        },
                    ));
                }

                let model = model.clone();
                menu.separator()
                    .item(PopupMenuItem::new("New project...").on_click(
                        move |_event, _window, cx| {
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
        let (
            selected_model,
            reasoning_effort,
            project_name,
            is_generating,
            session_status,
            pending_message,
            active_session_id,
        ) = {
            let state = self.model.read(cx);
            let project_name = state
                .active_work_dir
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "No project".to_string());
            (
                state.selected_model.clone(),
                state.reasoning_effort,
                project_name,
                state.is_generating,
                state.session_status.clone(),
                state.active_pending_composer_message().map(str::to_owned),
                state.active_session_id.clone(),
            )
        };
        let has_prompt =
            !self.input_state.read(cx).value().trim().is_empty() || !self.pasted_images.is_empty();
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

        let image_chips = self
            .pasted_images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let name = image.display_name.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.secondary)
                    .text_xs()
                    .child("▣")
                    .child(name)
                    .child(
                        Button::new(("remove-pasted-image", index))
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .tooltip("Remove image")
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                if index < this.pasted_images.len() {
                                    this.pasted_images.remove(index);
                                    cx.notify();
                                }
                            })),
                    )
            })
            .collect::<Vec<_>>();

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
                    Tag::new()
                        .child("Pending")
                        .with_variant(TagVariant::Secondary)
                        .small(),
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

        if self.branches.is_empty() {
            self.refresh_branches(cx);
        }
        let branch_model = self.model.clone();
        let branch_view = cx.entity().clone();
        let branches = self.branches.clone();
        let current_checkout = self.current_checkout.clone();
        let branch_error = self.branch_error.clone();
        let refresh_view = cx.entity().clone();
        let branch_picker = Button::new("composer-branch-picker")
            .label(current_checkout.as_deref().unwrap_or("Current checkout"))
            .dropdown_caret(true)
            .ghost()
            .on_click(move |_event, _window, cx| {
                refresh_view.update(cx, |this, cx| this.refresh_branches(cx));
            })
            .icon(Icon::default().path("icons/git/branch.svg"))
            .dropdown_menu(move |menu, _window, _cx| {
                if branches.is_empty() {
                    let message = branch_error
                        .as_deref()
                        .map(|error| format!("Git unavailable: {error}"))
                        .unwrap_or_else(|| "Loading local branches…".to_owned());
                    return menu.item(PopupMenuItem::new(message));
                }

                branches.iter().cloned().fold(menu, |menu, branch| {
                    let branch_view = branch_view.clone();
                    let branch_model = branch_model.clone();
                    menu.item(PopupMenuItem::new(branch.clone()).on_click(
                        move |_event, _window, cx| {
                            branch_view
                                .update(cx, |this, cx| this.checkout_branch(branch.clone(), cx));
                            branch_model.update(cx, |_, cx| cx.notify());
                        },
                    ))
                })
            });
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
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectModel(option.id.to_string()),
                                );
                                cx.notify();
                            });
                        }),
                )
            })
        });

        let effort_model = self.model.clone();
        let effort_picker = Button::new("composer-reasoning-effort-picker")
            .icon(Icon::default().path("icons/effort.svg"))
            .label(reasoning_effort.label())
            .tooltip(format!("Reasoning effort: {}", reasoning_effort.label()))
            .dropdown_caret(true)
            .ghost()
            .dropdown_menu(move |menu, _window, _cx| {
                [
                    ReasoningEffort::Off,
                    ReasoningEffort::Minimal,
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                    ReasoningEffort::XHigh,
                    ReasoningEffort::Max,
                ]
                .into_iter()
                .fold(menu, |menu, effort| {
                    let model = effort_model.clone();
                    menu.item(
                        PopupMenuItem::new(effort.label())
                            .checked(effort == reasoning_effort)
                            .on_click(move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectReasoningEffort(effort),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
                })
            });

        let input_value = self.input_state.read(cx).value().to_string();
        let command_menu = if input_value.starts_with('/') {
            let query = input_value[1..]
                .split_whitespace()
                .next()
                .unwrap_or_default();
            let commands = available_slash_commands(project_root.as_deref())
                .into_iter()
                .filter(|command| query.is_empty() || command.name.starts_with(query))
                .collect::<Vec<_>>();
            let command_count = commands.len();
            let shown_count = command_count.min(8);
            let has_commands = command_count > 0;
            let input_state = self.input_state.clone();
            div()
                .absolute()
                .left(px(16.0))
                .bottom(px(128.0))
                .w_full()
                .max_w(px(620.0))
                .max_h(px(286.0))
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .shadow_lg()
                .p_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(28.0))
                        .px_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("COMMANDS")
                        .child(format!("{shown_count}/{command_count}")),
                )
                .child(
                    div()
                        .when(!has_commands, |list| {
                            list.child(
                                div()
                                    .h(px(36.0))
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("No matching commands"),
                            )
                        })
                        .children(commands.into_iter().take(8).map(|command| {
                            let input_state = input_state.clone();
                            let value = format!("/{} ", command.name);
                            div()
                                .id(SharedString::from(format!(
                                    "composer-command-{}",
                                    command.name
                                )))
                                .h(px(30.0))
                                .flex()
                                .items_center()
                                .rounded_md()
                                .px_2()
                                .text_sm()
                                .hover(|style| style.bg(theme.list_hover))
                                .cursor_pointer()
                                .child(
                                    div()
                                        .w(px(112.0))
                                        .flex_none()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(format!("/{}", command.name)),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_color(theme.muted_foreground)
                                        .child(command.description),
                                )
                                .on_click(move |_event, window, cx| {
                                    input_state.update(cx, |state, cx| {
                                        state.set_value(&value, window, cx);
                                    });
                                })
                        })),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let token_usage = self.model.read(cx).current_session_token_usage();
        let context_max = crate::model_catalog::model_context_window(&selected_model);
        let percent = if context_max > 0 {
            ((token_usage.total_tokens as f64 / context_max as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        let meter_color = if percent >= 95.0 {
            theme.danger
        } else if percent >= 80.0 {
            theme.warning
        } else {
            theme.accent
        };
        let context_tooltip = format!(
            "Context window\n{} of {} tokens ({:.1}%)\nInput: {} • Output: {}",
            token_usage.total_tokens,
            context_max,
            percent,
            token_usage.input_tokens,
            token_usage.output_tokens
        );
        let context_meter = div()
            .id("context-meter-badge")
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(30.0))
            .rounded_full()
            .tooltip(move |window, cx| {
                let text = context_tooltip.clone();
                Tooltip::element(move |_window, _cx| div().child(text.clone())).build(window, cx)
            })
            .children(
                [
                    (px(12.0), px(1.0)),
                    (px(18.0), px(4.0)),
                    (px(22.0), px(10.0)),
                    (px(22.0), px(17.0)),
                    (px(18.0), px(23.0)),
                    (px(12.0), px(26.0)),
                    (px(6.0), px(23.0)),
                    (px(2.0), px(17.0)),
                    (px(2.0), px(10.0)),
                    (px(6.0), px(4.0)),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (left, top))| {
                    div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .size(px(4.0))
                        .rounded_full()
                        .bg(if percent >= ((index + 1) as f64 * 10.0) {
                            meter_color
                        } else {
                            theme.border
                        })
                }),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(format!("{percent:.0}")),
            );

        let stashed_draft = active_session_id
            .as_ref()
            .and_then(|id| self.model.read(cx).get_stashed_prompt(id).cloned());
        let stash_model = self.model.clone();
        let stash_input = self.input_state.clone();
        let stash_session_id = active_session_id.clone();
        let stash_banner = stashed_draft.map(|draft| {
            let restore_input = stash_input.clone();
            let restore_model = stash_model.clone();
            let restore_session_id = stash_session_id.clone();
            let dismiss_model = stash_model.clone();
            let dismiss_session_id = stash_session_id.clone();
            let preview_text = if draft.len() > 60 {
                format!("{}…", &draft[..60])
            } else {
                draft.clone()
            };
            div()
                .w_full()
                .mb_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.accent.opacity(0.3))
                .bg(theme.accent.opacity(0.1))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(Icon::default().path("icons/file.svg"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.foreground)
                                .truncate()
                                .child(format!("Stashed draft: \"{preview_text}\"")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("restore-stashed-draft")
                                .label("Restore Draft")
                                .small()
                                .primary()
                                .on_click(move |_event, window, cx| {
                                    if let Some(session_id) = &restore_session_id {
                                        if let Some(text) = restore_model.update(cx, |state, cx| {
                                            let popped = state.pop_stashed_prompt(session_id);
                                            cx.notify();
                                            popped
                                        }) {
                                            restore_input.update(cx, |input, cx| {
                                                input.set_value(text, window, cx);
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("dismiss-stashed-draft")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip("Discard stash")
                                .on_click(move |_event, _window, cx| {
                                    if let Some(session_id) = &dismiss_session_id {
                                        dismiss_model.update(cx, |state, cx| {
                                            state.clear_stashed_prompt(session_id);
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                )
        });

        let stash_button = {
            let do_stash_input = self.input_state.clone();
            let do_stash_model = self.model.clone();
            let do_stash_session_id = active_session_id.clone();
            Button::new("stash-prompt-btn")
                .icon(Icon::default().path("icons/folder.svg"))
                .tooltip("Stash draft")
                .ghost()
                .small()
                .disabled(is_generating || !has_prompt)
                .on_click(move |_event, window, cx| {
                    if let Some(session_id) = &do_stash_session_id {
                        let text = do_stash_input.read(cx).value().to_string();
                        if !text.trim().is_empty() {
                            do_stash_model.update(cx, |state, cx| {
                                state.stash_prompt(session_id, text);
                                cx.notify();
                            });
                            do_stash_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                        }
                    }
                })
        };

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
                    .relative()
                    .min_h(px(132.0))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .on_action(cx.listener(Self::paste_composer_clipboard))
                    .children(stash_banner)
                    .children(
                        (!image_chips.is_empty())
                            .then(|| div().flex().flex_wrap().gap_2().children(image_chips)),
                    )
                    .child(command_menu)
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
                            .child(effort_picker)
                            .child(context_meter)
                            .child(div().flex_1())
                            .child(stash_button)
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
                                    .on_click(cx.listener(move |this, _event, window, cx| {
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
                                        if !text.trim().is_empty() || !this.pasted_images.is_empty()
                                        {
                                            let images = std::mem::take(&mut this.pasted_images);
                                            model.update(cx, |state, cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::SendPromptWithImages {
                                                        text,
                                                        images,
                                                    },
                                                );
                                                cx.notify();
                                            });
                                            input_state.update(cx, |state, cx| {
                                                state.set_value("", window, cx);
                                            });
                                            this.scroll_handle.scroll_to_bottom();
                                            cx.notify();
                                        }
                                    })),
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
                    .child(div().flex_1())
                    .child(branch_picker)
                    .children(
                        session_status
                            .filter(|status| !status.starts_with("Working"))
                            .map(|status| {
                                div().flex().items_center().gap_2().child("·").child(status)
                            }),
                    ),
            )
    }
}

impl Render for ChatListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (messages, is_new_task, active_plan) = {
            let state = self.model.read(cx);
            (
                state.messages.clone(),
                state.is_new_task,
                state.active_plan.clone(),
            )
        };
        let theme = cx.theme().colors;
        let transcript_rows = self.render_transcript_rows(&messages, cx);

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
                    .overflow_y_scroll()
                    .vertical_scrollbar(&self.scroll_handle)
                    .pt_3()
                    .pb_6()
                    .children(transcript_rows)
                    .into_any_element()
            })
            .children(self.render_plan_tracker(&active_plan, cx))
            .child(self.render_composer(cx))
    }
}
