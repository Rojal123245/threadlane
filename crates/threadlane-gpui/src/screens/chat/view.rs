use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use std::time::Duration;

use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::collapsible::Collapsible;
use gpui_component::hover_card::HoverCard;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::progress::ProgressCircle;
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Disableable, Icon, IconName, Selectable, Sizable, WindowExt};

use crate::app::{actions::AppAction, controller};
use crate::screens::editor::EditorView;
use crate::state::{
    compute_message_page, AppState, ChatMessageInfo, MessageRole, ToolActivityInfo, TrajectoryEntry,
};
use threadlane_session::commands::{available_slash_commands, SlashCommandInfo};
use threadlane_session::{ImageAttachment, PlanItemStatus, ReasoningEffort, SessionPlan};

actions!(threadlane_composer, [PasteClipboard]);

const INPUT_KEY_CONTEXT: &str = "Input";

const CHAT_CONTENT_MAX_WIDTH: f32 = 1040.0;
const USER_BUBBLE_MAX_WIDTH: f32 = 680.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CentralTab {
    #[default]
    Chat,
    Trajectory,
    Editor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum TrajectoryMode {
    #[default]
    Execution,
    Requests,
    ModelContext,
    DurableEvents,
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum TrajectoryInspectorTab {
    #[default]
    Overview,
    Preview,
    Raw,
    Source,
}

fn should_use_markdown(streaming: bool) -> bool {
    !streaming
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TranscriptRow {
    Message(usize),
    Activities(Range<usize>),
    Working,
}

fn is_activity_only(message: &ChatMessageInfo) -> bool {
    message.role == MessageRole::Assistant
        && message.content.is_empty()
        && message.reasoning_content.is_none()
        && message
            .tool_activities
            .iter()
            .any(|activity| activity.title != "update_plan")
}

fn build_transcript_rows(messages: &[ChatMessageInfo], generating: bool) -> Vec<TranscriptRow> {
    let mut rows = Vec::with_capacity(messages.len().saturating_add(1));
    let mut index = 0;
    while index < messages.len() {
        if !is_activity_only(&messages[index]) {
            rows.push(TranscriptRow::Message(index));
            index += 1;
            continue;
        }

        let start = index;
        while index < messages.len() && is_activity_only(&messages[index]) {
            index += 1;
        }
        rows.push(TranscriptRow::Activities(start..index));
    }
    if generating {
        rows.push(TranscriptRow::Working);
    }
    rows
}

fn grouped_tool_activities(
    messages: &[ChatMessageInfo],
) -> impl Iterator<Item = &ToolActivityInfo> + Clone {
    messages
        .iter()
        .flat_map(|message| message.tool_activities.iter())
        .filter(|activity| activity.title != "update_plan")
}

fn format_trajectory_raw_json(entry: &TrajectoryEntry) -> String {
    serde_json::to_string_pretty(entry).unwrap_or_else(|_| entry.detail.clone())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrajectoryCacheKey {
    revision: u64,
    mode: TrajectoryMode,
    query: String,
    category: Option<String>,
    lane: Option<String>,
}

struct TrajectoryRenderCache {
    key: TrajectoryCacheKey,
    all_entries: Vec<TrajectoryEntry>,
    raw_json: Vec<String>,
    categories: Arc<Vec<String>>,
    lanes: Arc<Vec<String>>,
    lane_latest: Arc<std::collections::BTreeMap<String, String>>,
    filtered_indices: Vec<usize>,
}

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
    model: Entity<AppState>,
    pub(crate) input_state: Entity<TextareaState>,
    pub(crate) header_left_padding: Pixels,
    transcript_list_state: ListState,
    transcript_messages: Arc<Vec<ChatMessageInfo>>,
    transcript_rows: Vec<TranscriptRow>,
    transcript_generating: bool,
    trajectory_scroll_handle: ScrollHandle,
    expanded_activity_groups: HashSet<String>,
    markdown_states: HashMap<String, (usize, Entity<TextViewState>)>,
    pasted_images: Vec<ImageAttachment>,
    last_session_key: Option<(std::path::PathBuf, String)>,
    initial_scroll_frames: u8,
    older_load_pending: bool,
    current_tab: CentralTab,
    editor: Entity<EditorView>,
    trajectory_mode: TrajectoryMode,
    trajectory_search: String,
    trajectory_search_input: Entity<InputState>,
    trajectory_category: Option<String>,
    trajectory_lane: Option<String>,
    selected_trajectory_index: Option<usize>,
    trajectory_inspector_tab: TrajectoryInspectorTab,
    trajectory_cache: Option<TrajectoryRenderCache>,
    slash_command_cache: Option<(
        Option<std::path::PathBuf>,
        std::time::Instant,
        Vec<SlashCommandInfo>,
    )>,
    _subscriptions: Vec<Subscription>,
}

impl ChatListView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let transcript_list_state = ListState::new(0, ListAlignment::Bottom, px(600.0));
        transcript_list_state.set_follow_mode(FollowMode::Tail);
        let trajectory_scroll_handle = ScrollHandle::new();
        let input_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Do anything...")
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .soft_wrap(true)
        });

        let trajectory_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search trajectory…"));

        let editor = cx.new(|cx| EditorView::new(model.clone(), window, cx));

        let sub1 = cx.observe(&model, |this, model, cx| {
            if let Some(target) =
                model.update(cx, |state, _cx| state.requested_editor_target.take())
            {
                this.current_tab = CentralTab::Editor;
                match target {
                    crate::state::RequestedEditorTarget::File(path) => {
                        this.editor.update(cx, |editor, cx| {
                            editor.open_file(&path, cx);
                        });
                    }
                    crate::state::RequestedEditorTarget::Diff {
                        project,
                        path,
                        content,
                    } => {
                        if model.read(cx).active_work_dir.as_ref() == Some(&project) {
                            this.editor.update(cx, |editor, cx| {
                                editor.open_diff(&path, &content, cx);
                            });
                        }
                    }
                }
            }
            cx.notify();
        });

        let sub_editor = cx.observe(&editor, |_this, _editor, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let submit_list_state = transcript_list_state.clone();
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
                        submit_list_state.scroll_to_end();
                        cx.notify();
                    }
                }
            },
        );

        let stream_model = model.clone();
        cx.spawn(async move |this, cx| {
            let mut settle_frames = 0_u8;
            loop {
                // Event-driven pacing: check quickly when generating,
                // slow down when idle.
                let interval = if settle_frames > 0 {
                    Duration::from_millis(30) // ~33fps for smooth streaming without UI thread starvation
                } else {
                    Duration::from_millis(100)
                };
                cx.background_executor().timer(interval).await;

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
                }

                if !changed && settle_frames > 0 {
                    let _ = this.update(cx, |_this, cx| cx.notify());
                    settle_frames = settle_frames.saturating_sub(1);
                }
            }
        })
        .detach();

        let sub3 = cx.observe(&trajectory_search_input, |this, input, cx| {
            this.trajectory_search = input.read(cx).value().to_string();
            cx.notify();
        });

        Self {
            model,
            input_state,
            header_left_padding: px(14.0),
            transcript_list_state,
            transcript_messages: Arc::new(Vec::new()),
            transcript_rows: Vec::new(),
            transcript_generating: false,
            trajectory_scroll_handle,
            expanded_activity_groups: HashSet::new(),
            markdown_states: HashMap::new(),
            pasted_images: Vec::new(),
            last_session_key: None,
            initial_scroll_frames: 0,
            older_load_pending: false,
            current_tab: CentralTab::Chat,
            editor,
            trajectory_mode: TrajectoryMode::Execution,
            trajectory_search: String::new(),
            trajectory_search_input,
            trajectory_category: None,
            trajectory_lane: None,
            selected_trajectory_index: None,
            trajectory_inspector_tab: TrajectoryInspectorTab::Overview,
            trajectory_cache: None,
            slash_command_cache: None,
            _subscriptions: vec![sub1, sub2, sub3, sub_editor],
        }
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

    pub(crate) fn set_composer_text(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.current_tab = CentralTab::Chat;
        self.input_state.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        cx.notify();
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
        let editor_tab_count = self.editor.read(cx).tab_count();
        let editor_label = if editor_tab_count > 0 {
            format!("Editor ({editor_tab_count})")
        } else {
            "Editor".to_string()
        };

        div()
            .h(px(52.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .pl(self.header_left_padding)
            .pr(px(128.0))
            .border_b_1()
            .border_color(theme.title_bar_border)
            .bg(theme.title_bar)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_start()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(active_title),
                    ),
            )
            .child(
                div()
                    .h(px(30.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p(px(2.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.4))
                    .child(
                        Button::new("trajectory-tab-events")
                            .icon(Icon::default().path("icons/tabs/trajectory.svg"))
                            .label("Trajectory")
                            .tooltip("Trajectory (Execution & Diagnostics)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Trajectory)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Trajectory;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("trajectory-tab-chat")
                            .icon(Icon::default().path("icons/tabs/chat.svg"))
                            .label("Chat")
                            .tooltip("Chat (Conversation & Turn History)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Chat)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Chat;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("trajectory-tab-editor")
                            .icon(Icon::default().path("icons/tabs/editor.svg"))
                            .label(editor_label)
                            .tooltip("Editor (Code & Diff Review)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Editor)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Editor;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1())
    }

    /// Renders the 16px status circle used for a plan step: a bordered ✓ for
    /// completed, a spinner for in-progress (active generation), a static dot for in-progress (idle), and an empty ring for pending.
    fn plan_step_marker(
        status: PlanItemStatus,
        is_generating: bool,
        colors: gpui_component::ThemeColor,
    ) -> AnyElement {
        match status {
            PlanItemStatus::Completed => div()
                .size(px(16.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(colors.success)
                .text_size(px(10.0))
                .font_weight(FontWeight::BOLD)
                .text_color(colors.success)
                .child("✓")
                .into_any_element(),
            PlanItemStatus::InProgress => {
                if is_generating {
                    div()
                        .size(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(colors.primary)
                        .child(gpui_component::spinner::Spinner::new().xsmall())
                        .into_any_element()
                } else {
                    div()
                        .size(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.primary)
                        .child(div().size(px(6.0)).rounded_full().bg(colors.primary))
                        .into_any_element()
                }
            }
            PlanItemStatus::Pending => div()
                .size(px(16.0))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(colors.muted_foreground)
                .into_any_element(),
        }
    }

    fn render_plan_tracker(
        &self,
        plan: &SessionPlan,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if plan.items.is_empty() {
            return None;
        }

        let is_generating = self.model.read(cx).is_generating;
        let theme = cx.theme().colors;
        let completed = plan
            .items
            .iter()
            .filter(|item| item.status == PlanItemStatus::Completed)
            .count();
        let total = plan.items.len();
        let current_step = plan
            .items
            .iter()
            .position(|item| item.status == PlanItemStatus::InProgress)
            .or_else(|| {
                plan.items
                    .iter()
                    .position(|item| item.status == PlanItemStatus::Pending)
            })
            .map(|index| index + 1)
            .unwrap_or(total);
        let is_complete = completed == total;
        let content_plan = plan.clone();

        Some(
            HoverCard::new("session-plan-hover-card")
                .w_full()
                .flex_none()
                .anchor(Anchor::BottomCenter)
                .close_delay(Duration::from_millis(700))
                .trigger(
                    div().w_full().flex().justify_center().py_1().child(
                        Button::new("session-plan-tracker").ghost().child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Self::plan_step_marker(
                                    if is_complete {
                                        PlanItemStatus::Completed
                                    } else {
                                        PlanItemStatus::InProgress
                                    },
                                    is_generating,
                                    theme,
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.muted_foreground)
                                        .child(format!("Step {current_step} / {total}")),
                                ),
                        ),
                    ),
                )
                .content(move |_state, _window, _cx| {
                    let colors = theme;
                    let rows = content_plan.items.iter().enumerate().map(|(index, item)| {
                        let marker = Self::plan_step_marker(item.status, is_generating, colors);
                        div().flex().items_start().gap_2().child(marker).child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .text_sm()
                                .text_color(colors.foreground)
                                .child(format!("{}. {}", index + 1, item.step)),
                        )
                    });
                    div()
                        .w(px(520.0))
                        .max_w(px(CHAT_CONTENT_MAX_WIDTH - 32.0))
                        .max_h(px(360.0))
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
        let display_summary = activity.display_summary.clone();

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
                            .child(display_summary),
                    )
                    .children(has_detail.then(|| {
                        Icon::new(if activity.is_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground)
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
        messages: &[ChatMessageInfo],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const RECENT_ACTIVITY_LIMIT: usize = 4;

        let theme = cx.theme().colors;
        let activities = grouped_tool_activities(messages);
        let group_id = activities
            .clone()
            .next()
            .map(|activity| activity.id.clone())
            .unwrap_or_else(|| "empty".into());
        let is_expanded = self.expanded_activity_groups.contains(&group_id);
        let hidden_count = activities
            .clone()
            .count()
            .saturating_sub(RECENT_ACTIVITY_LIMIT);
        let visible_start = if is_expanded { 0 } else { hidden_count };
        let activity_rows = activities
            .skip(visible_start)
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

    fn render_trajectory(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let revision = self.model.read(cx).trajectory_revision();
        let key = TrajectoryCacheKey {
            revision,
            mode: self.trajectory_mode,
            query: self.trajectory_search.to_lowercase(),
            category: self.trajectory_category.clone(),
            lane: self.trajectory_lane.clone(),
        };
        if self
            .trajectory_cache
            .as_ref()
            .is_none_or(|cache| cache.key != key)
        {
            let all_entries = match self.trajectory_mode {
                TrajectoryMode::Execution | TrajectoryMode::Requests => {
                    self.model.read(cx).active_trajectory().to_vec()
                }
                TrajectoryMode::ModelContext => {
                    self.model.read(cx).active_model_context_diagnostics()
                }
                TrajectoryMode::DurableEvents => {
                    self.model.read(cx).active_durable_event_diagnostics()
                }
                TrajectoryMode::Recovery => self.model.read(cx).active_recovery_diagnostics(),
            };
            let raw_json = all_entries
                .iter()
                .map(format_trajectory_raw_json)
                .collect::<Vec<_>>();
            let mut categories = all_entries
                .iter()
                .map(|entry| entry.category.clone())
                .collect::<Vec<_>>();
            categories.sort();
            categories.dedup();
            let mut lane_latest = std::collections::BTreeMap::new();
            for entry in &all_entries {
                if let Some(lane) = &entry.lane {
                    lane_latest.insert(lane.clone(), entry.summary.clone());
                }
            }
            let lanes = lane_latest.keys().cloned().collect();
            let filtered_indices = all_entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    key.category
                        .as_ref()
                        .is_none_or(|category| &entry.category == category)
                        && key
                            .lane
                            .as_ref()
                            .is_none_or(|lane| entry.lane.as_ref() == Some(lane))
                        && (key.query.is_empty()
                            || [
                                entry.category.as_str(),
                                entry.summary.as_str(),
                                entry.detail.as_str(),
                                entry.lane.as_deref().unwrap_or(""),
                                entry.correlation_id.as_deref().unwrap_or(""),
                            ]
                            .iter()
                            .any(|value| value.to_lowercase().contains(&key.query)))
                })
                .map(|(index, _)| index)
                .collect();
            self.trajectory_cache = Some(TrajectoryRenderCache {
                key,
                all_entries,
                raw_json,
                categories: Arc::new(categories),
                lanes: Arc::new(lanes),
                lane_latest: Arc::new(lane_latest),
                filtered_indices,
            });
        }
        let cache = self.trajectory_cache.as_ref().expect("trajectory cache");
        let all_entries = &cache.all_entries;
        let categories = Arc::clone(&cache.categories);
        let lanes = Arc::clone(&cache.lanes);
        let lane_latest = Arc::clone(&cache.lane_latest);
        let entries = &cache.filtered_indices;
        let theme = cx.theme().colors;
        if entries.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No canonical trajectory events have been observed in this session yet.")
                .into_any_element();
        }
        let selected_index = self.selected_trajectory_index;
        let selected_entry = selected_index
            .and_then(|index| all_entries.get(index))
            .cloned();
        let selected_raw_json = selected_index
            .and_then(|index| cache.raw_json.get(index))
            .cloned();
        let mut rows = Vec::new();
        let mut previous_turn = None;
        let mut previous_request = None;
        let mut request_input_seen = false;
        for &all_index in entries {
            let entry = &all_entries[all_index];
            if self.trajectory_mode == TrajectoryMode::Requests && entry.request != previous_request
            {
                if let Some(request) = entry.request {
                    request_input_seen = false;
                    rows.push(
                        div()
                            .h(px(28.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(theme.border.opacity(0.65))
                            .bg(theme.muted.opacity(0.35))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.accent)
                            .child(format!("Request #{request}"))
                            .into_any_element(),
                    );
                }
                previous_request = entry.request;
            }
            if self.trajectory_mode == TrajectoryMode::Requests
                && entry.request.is_some()
                && !request_input_seen
                && entry.category == "Input"
            {
                request_input_seen = true;
            } else if self.trajectory_mode == TrajectoryMode::Requests
                && entry.request.is_some()
                && !request_input_seen
            {
                rows.push(
                    div()
                        .h(px(20.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(theme.border.opacity(0.35))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child("Setup")
                        .into_any_element(),
                );
                request_input_seen = true;
            }
            if self.trajectory_mode != TrajectoryMode::Requests && entry.turn != previous_turn {
                if let Some(turn) = entry.turn {
                    rows.push(
                        div()
                            .h(px(22.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(theme.border.opacity(0.5))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("Turn {turn}"))
                            .into_any_element(),
                    );
                }
                previous_turn = entry.turn;
            }
            let seq = entry.seq;
            let selected = Some(all_index) == self.selected_trajectory_index;
            let preview = if entry.detail.trim().is_empty() {
                entry.summary.clone()
            } else {
                format!("{}  {}", entry.summary, entry.detail.replace('\n', " "))
            };
            let view = cx.entity().clone();
            let (badge_bg, badge_fg, badge_label): (Hsla, Hsla, SharedString) =
                match entry.category.as_str() {
                    "Tool" | "Tool runtime" => {
                        (theme.warning.opacity(0.18), theme.warning, "TOOL".into())
                    }
                    "Provider" => (
                        theme.primary.opacity(0.18),
                        theme.primary,
                        "PROVIDER".into(),
                    ),
                    "Context Manifest" | "Manifest" => (
                        theme.accent.opacity(0.14),
                        theme.muted_foreground,
                        "MANIFEST".into(),
                    ),
                    "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                    "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                    "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                    "Input" => (theme.muted.opacity(0.8), theme.foreground, "INPUT".into()),
                    "Assistant" => (
                        theme.muted.opacity(0.8),
                        theme.foreground,
                        "ASSISTANT".into(),
                    ),
                    "Permission" => (
                        theme.warning.opacity(0.18),
                        theme.warning,
                        "PERMISSION".into(),
                    ),
                    "Subagent" => (
                        theme.primary.opacity(0.16),
                        theme.primary,
                        "SUBAGENT".into(),
                    ),
                    _ => (
                        theme.muted.opacity(0.5),
                        theme.muted_foreground,
                        entry.category.clone().into(),
                    ),
                };
            let dot_color = if entry.diagnostics.is_anomaly || entry.category == "Anomaly" {
                theme.warning
            } else if entry.category == "Error"
                || entry.detail.contains("Failed")
                || entry.detail.contains("Error")
                || entry.diagnostics.status.as_deref() == Some("Failed")
                || entry.diagnostics.status.as_deref() == Some("failed")
            {
                theme.danger
            } else if entry.category == "Tool" || entry.category == "Tool runtime" {
                theme.warning
            } else if entry.category == "Request" {
                theme.primary
            } else {
                theme.muted_foreground
            };
            rows.push(
                div()
                    .id(SharedString::from(format!("trajectory-{all_index}")))
                    .h(px(34.0))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.45))
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(theme.accent.opacity(0.16))
                            .border_l_2()
                            .border_color(theme.accent)
                    })
                    .hover(|style| style.bg(theme.muted.opacity(0.65)))
                    .child(div().size(px(6.0)).flex_none().rounded_full().bg(dot_color))
                    .child(
                        div()
                            .w(px(84.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .bg(badge_bg)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(badge_fg)
                            .child(badge_label),
                    )
                    .child(div().min_w_0().flex_1().text_sm().truncate().child(preview))
                    .children(entry.diagnostics.exit_code.map(|code| {
                        let is_ok = code == 0;
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(if is_ok {
                                theme.success.opacity(0.15)
                            } else {
                                theme.danger.opacity(0.15)
                            })
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_ok { theme.success } else { theme.danger })
                            .child(format!("exit {code}"))
                    }))
                    .children(entry.diagnostics.duration_ms.map(|dur| {
                        let dur_str = if dur < 1000 {
                            format!("{dur}ms")
                        } else {
                            format!("{:.1}s", dur as f64 / 1000.0)
                        };
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(theme.muted.opacity(0.8))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(dur_str)
                    }))
                    .children(entry.lane.clone().map(|lane| {
                        div()
                            .max_w(px(110.0))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(lane)
                    }))
                    .children(seq.map(|seq| {
                        div()
                            .w(px(52.0))
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("#{seq}"))
                    }))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.selected_trajectory_index = Some(all_index);
                            this.trajectory_inspector_tab = TrajectoryInspectorTab::Overview;
                            cx.notify();
                        })
                    })
                    .into_any_element(),
            );
        }
        let inspector_tab = self.trajectory_inspector_tab;
        let inspector = selected_entry.map(|entry| {
            let close_view = cx.entity().clone();
            let inspector_view = cx.entity().clone();
            let model_visible = entry.diagnostics.model_visible || matches!(
                entry.category.as_str(),
                "Input" | "Assistant" | "Context" | "Context Manifest" | "Tool"
            );
            let provenance = match entry.category.as_str() {
                "Input" => "User transcript · model-visible",
                "Assistant" => "Assistant transcript · model-visible",
                "Context" | "Context Manifest" => "Runtime context package · model-visible",
                "Tool" | "Tool runtime" => "Tool transcript · model-visible",
                "Anomaly" => "Automated diagnostic anomaly · durable",
                "Error" => "Runtime diagnostic · durable",
                _ => "Runtime lifecycle record · durable",
            };
            let mut metadata_items = vec![
                entry.seq.map(|value| ("Sequence", format!("#{value}"))),
                entry.request.map(|value| ("Request", format!("#{value}"))),
                entry.turn.map(|value| ("Turn", value.to_string())),
                entry.run_id.clone().map(|value| ("Run", value)),
                entry.lane.clone().map(|value| ("Lane", value)),
                entry.correlation_id.clone().map(|value| ("Call / Correlation", value)),
                entry.diagnostics.status.clone().map(|value| ("Status", value)),
                entry.diagnostics.duration_ms.map(|value| {
                    (
                        "Duration",
                        if value < 1000 {
                            format!("{value} ms")
                        } else {
                            format!("{:.2} s", value as f64 / 1000.0)
                        },
                    )
                }),
                entry.diagnostics.exit_code.map(|value| ("Exit Code", value.to_string())),
                entry.diagnostics.output_bytes.map(|value| ("Output Size", format!("{value} bytes"))),
                entry.diagnostics.token_estimate.map(|value| ("Est. Tokens", format!("~{value}"))),
                entry.diagnostics.items_count.map(|value| ("Item Count", value.to_string())),
            ];
            if !entry.diagnostics.files_mutated.is_empty() {
                metadata_items.push(Some(("Files Mutated", entry.diagnostics.files_mutated.join(", "))));
            }
            if !entry.diagnostics.commands_executed.is_empty() {
                metadata_items.push(Some(("Commands Executed", entry.diagnostics.commands_executed.join(", "))));
            }
            let metadata = metadata_items.into_iter().flatten();
            let (header_bg, header_fg, header_tag): (Hsla, Hsla, SharedString) = match entry.category.as_str() {
                "Tool" | "Tool runtime" => (theme.warning.opacity(0.18), theme.warning, "TOOL".into()),
                "Provider" => (theme.primary.opacity(0.18), theme.primary, "PROVIDER".into()),
                "Context Manifest" | "Manifest" => (theme.accent.opacity(0.14), theme.muted_foreground, "MANIFEST".into()),
                "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                _ => (theme.muted.opacity(0.5), theme.muted_foreground, entry.category.clone().into()),
            };
            div()
                .w(px(410.0))
                .min_w(px(320.0))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(
                    div()
                        .h(px(48.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(header_bg)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(header_fg)
                                .child(header_tag),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(entry.summary.clone()),
                        )
                        .children(entry.diagnostics.duration_ms.map(|dur| {
                            let dur_str = if dur < 1000 { format!("{dur}ms") } else { format!("{:.1}s", dur as f64 / 1000.0) };
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme.muted.opacity(0.7))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(dur_str)
                        }))
                        .child(
                            Button::new("copy-trajectory-row")
                                .ghost()
                                .xsmall()
                                .label("📋")
                                .tooltip("Copy trajectory entry")
                                .on_click({
                                    let text = format!(
                                        "seq:{:?} turn:{:?} category:{} summary:{} detail:{} lane:{:?} run:{:?} call:{:?}",
                                        entry.seq, entry.turn, entry.category, entry.summary,
                                        entry.detail, entry.lane, entry.run_id, entry.correlation_id,
                                    );
                                    move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                        window.push_notification(
                                            Notification::info("Copied trajectory entry"),
                                            cx,
                                        );
                                    }
                                }),
                        )
                        .child(
                            Button::new("close-trajectory-inspector")
                                .ghost()
                                .xsmall()
                                .label("×")
                                .tooltip("Close inspector")
                                .on_click(move |_, _, cx| {
                                    close_view.update(cx, |this, cx| {
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    })
                                }),
                        ),
                )
                .child(
                    div()
                        .h(px(38.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .children([
                            ("Overview", TrajectoryInspectorTab::Overview),
                            ("Preview", TrajectoryInspectorTab::Preview),
                            ("Raw", TrajectoryInspectorTab::Raw),
                            ("Source", TrajectoryInspectorTab::Source),
                        ]
                        .into_iter()
                        .map(|(label, tab)| {
                            let view = inspector_view.clone();
                            Button::new(SharedString::from(format!("trajectory-inspector-{label}")))
                                .ghost()
                                .small()
                                .selected(inspector_tab == tab)
                                .label(label)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_inspector_tab = tab;
                                        cx.notify();
                                    })
                                })
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(match inspector_tab {
                            TrajectoryInspectorTab::Overview => div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(
                                    div()
                                        .p_3()
                                        .rounded_lg()
                                        .bg(theme.muted.opacity(0.3))
                                        .border_1()
                                        .border_color(theme.border.opacity(0.5))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .children(metadata.map(|(label, value)| {
                                            div()
                                                .flex()
                                                .gap_2()
                                                .text_sm()
                                                .child(
                                                    div()
                                                        .w(px(110.0))
                                                        .flex_none()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme.muted_foreground)
                                                        .child(label),
                                                )
                                                .child(div().min_w_0().flex_1().text_xs().child(value.clone()))
                                        })),
                                )
                                .children(entry.diagnostics.raw.as_ref().map(|raw_args| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("INPUT ARGUMENTS"))
                                        .child(TextView::markdown(
                                            format!("trajectory-args-{}", entry.seq.unwrap_or(0)),
                                            format!("```json\n{}\n```", raw_args),
                                        ).selectable(true))
                                }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("VISIBILITY"))
                                .child(div().text_sm().child(if model_visible { "Model-visible transcript/context" } else { "Runtime-only durable diagnostic" }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("SUMMARY"))
                                .child(div().text_sm().child(entry.summary.clone()))
                                .into_any_element(),
                            TrajectoryInspectorTab::Preview => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("OUTPUT PREVIEW"))
                                .child(
                                    if entry.detail.is_empty() {
                                        div().text_sm().child("No preview content is available for this event.").into_any_element()
                                    } else {
                                        TextView::markdown(
                                            format!("trajectory-preview-{}", entry.seq.unwrap_or(0)),
                                            entry.detail.clone(),
                                        )
                                        .selectable(true)
                                        .into_any_element()
                                    },
                                )
                                .into_any_element(),
                            TrajectoryInspectorTab::Raw => {
                                let raw_json = selected_raw_json.clone().unwrap_or_default();
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("CANONICAL PROJECTION (JSON)"))
                                    .child(TextView::markdown(
                                        format!("trajectory-raw-{}", entry.seq.unwrap_or(0)),
                                        format!("```json\n{raw_json}\n```"),
                                    ).selectable(true))
                                    .into_any_element()
                            }
                            TrajectoryInspectorTab::Source => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PROVENANCE"))
                                .child(div().text_sm().child(entry.diagnostics.source.clone().unwrap_or_else(|| provenance.to_string())))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("LINEAGE"))
                                .child(div().text_sm().child(format!(
                                    "Request {} · Turn {} · Lane {}",
                                    entry.request.map_or("—".to_string(), |request| format!("#{request}")),
                                    entry.turn.map_or("—".to_string(), |turn| turn.to_string()),
                                    entry.lane.as_deref().unwrap_or("—"),
                                )))
                                .children(entry.diagnostics.parent_id.as_ref().map(|p| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PARENT ENTRY"))
                                        .child(div().text_sm().font_family("monospace").child(p.clone()))
                                }))
                                .children(entry.diagnostics.result_id.as_ref().map(|r| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("RESULT ENTRY"))
                                        .child(div().text_sm().font_family("monospace").child(r.clone()))
                                }))
                                .children(entry.correlation_id.clone().map(|id| div().text_sm().child(format!("Correlation: {id}"))))
                                .into_any_element(),
                        }),
                )
        });
        let overview_lane = |label: &'static str, category: &'static str, color: Hsla| {
            let markers = all_entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| match category {
                    "Input" => matches!(
                        entry.category.as_str(),
                        "Input" | "Context" | "Context Manifest" | "Queue" | "Request"
                    ),
                    "Model" => matches!(
                        entry.category.as_str(),
                        "Operation" | "Step" | "Retry" | "Turn" | "Error" | "Provider" | "Anomaly"
                    ),
                    _ => matches!(entry.category.as_str(), "Tool" | "Tool runtime"),
                })
                .map(|(index, _)| {
                    let position = index * 48 / all_entries.len().max(1);
                    (position, color)
                })
                .collect::<HashMap<_, _>>();
            div()
                .h(px(18.0))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(48.0))
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(12.0))
                        .flex()
                        .items_end()
                        .gap(px(2.0))
                        .children((0..48).map(|index| {
                            div()
                                .flex_1()
                                .h(if markers.contains_key(&index) {
                                    px(10.0)
                                } else {
                                    px(2.0)
                                })
                                .rounded_sm()
                                .bg(markers
                                    .get(&index)
                                    .copied()
                                    .unwrap_or(theme.border.opacity(0.35)))
                        })),
                )
        };
        let overview = div()
            .h(px(58.0))
            .flex_none()
            .flex()
            .flex_col()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(overview_lane("Input", "Input", theme.success))
            .child(overview_lane("Model", "Model", theme.primary))
            .child(overview_lane("Tools", "Tools", theme.warning));
        let category_label = self
            .trajectory_category
            .clone()
            .unwrap_or_else(|| "All events".into());
        let lane_label = self
            .trajectory_lane
            .clone()
            .unwrap_or_else(|| format!("{} lanes", lanes.len()));
        let category_view = cx.entity().clone();
        let lane_view = cx.entity().clone();
        let mode_view = cx.entity().clone();
        let mode_label = match self.trajectory_mode {
            TrajectoryMode::Execution => "Execution",
            TrajectoryMode::Requests => "Requests",
            TrajectoryMode::ModelContext => "Model Context",
            TrajectoryMode::DurableEvents => "Durable Events",
            TrajectoryMode::Recovery => "Recovery",
        };
        let toolbar = div()
            .h(px(38.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                Button::new("trajectory-mode-filter")
                    .ghost()
                    .small()
                    .label(mode_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for (label, mode) in [
                            ("Execution", TrajectoryMode::Execution),
                            ("Requests", TrajectoryMode::Requests),
                            ("Model Context", TrajectoryMode::ModelContext),
                            ("Durable Events", TrajectoryMode::DurableEvents),
                            ("Recovery", TrajectoryMode::Recovery),
                        ] {
                            let view = mode_view.clone();
                            menu =
                                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_mode = mode;
                                        this.trajectory_category = None;
                                        this.trajectory_lane = None;
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    });
                                }));
                        }
                        menu
                    }),
            )
            .child(
                Button::new("trajectory-category-filter")
                    .ghost()
                    .small()
                    .label(category_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let all_view = category_view.clone();
                        let mut menu = menu.item(PopupMenuItem::new("All events").on_click(
                            move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_category = None;
                                    cx.notify();
                                });
                            },
                        ));
                        for category in categories.iter().cloned() {
                            let selected = category.clone();
                            let view = category_view.clone();
                            menu = menu.item(PopupMenuItem::new(category).on_click(
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_category = Some(selected.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .children((lanes.len() > 1).then(|| {
                Button::new("trajectory-lane-filter")
                    .ghost()
                    .small()
                    .label(lane_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let all_view = lane_view.clone();
                        let mut menu =
                            menu.item(PopupMenuItem::new("All lanes").on_click(move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_lane = None;
                                    cx.notify();
                                });
                            }));
                        for lane in lanes.iter().cloned() {
                            let selected = lane.clone();
                            let view = lane_view.clone();
                            let latest = lane_latest.get(&lane).cloned().unwrap_or_default();
                            menu = menu.item(
                                PopupMenuItem::new(format!("{lane} — {latest}")).on_click(
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.trajectory_lane = Some(selected.clone());
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    })
            }))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(280.0))
                    .h(px(32.0))
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(Input::new(&self.trajectory_search_input).appearance(false)),
            );
        let tool_count = all_entries
            .iter()
            .filter(|e| e.category == "Tool" || e.category == "Tool runtime")
            .count();
        let total_dur_ms: u64 = all_entries
            .iter()
            .filter_map(|e| e.diagnostics.duration_ms)
            .sum();
        let dur_label = if total_dur_ms < 1000 {
            format!("{total_dur_ms}ms total")
        } else {
            format!("{:.2}s total", total_dur_ms as f64 / 1000.0)
        };
        let anomaly_count = all_entries
            .iter()
            .filter(|e| e.diagnostics.is_anomaly || e.category == "Anomaly")
            .count();
        let max_turn = all_entries.iter().filter_map(|e| e.turn).max().unwrap_or(0);

        let stats_bar = div()
            .h(px(26.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .border_b_1()
            .border_color(theme.border.opacity(0.4))
            .bg(theme.muted.opacity(0.15))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("{max_turn}")),
                    )
                    .child("turns"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("{tool_count}")),
                    )
                    .child("tool calls"),
            )
            .child(
                div().flex().items_center().gap_1().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(dur_label),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(6.0)).rounded_full().bg(if anomaly_count > 0 {
                        theme.warning
                    } else {
                        theme.success
                    }))
                    .child(format!("{anomaly_count} anomalies")),
            );

        div()
            .id("session-trajectory")
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(overview)
            .child(toolbar)
            .child(stats_bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("trajectory-events-container")
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                div()
                                    .id("trajectory-events-scroll")
                                    .size_full()
                                    .track_scroll(&self.trajectory_scroll_handle)
                                    .overflow_y_scroll()
                                    .child(div().w_full().children(rows)),
                            )
                            .child(div().absolute().inset_0().child(
                                gpui_component::scroll::Scrollbar::vertical(
                                    &self.trajectory_scroll_handle,
                                ),
                            )),
                    )
                    .children(inspector),
            )
            .into_any_element()
    }

    fn sync_transcript_rows(
        &mut self,
        messages: Arc<Vec<ChatMessageInfo>>,
        generating: bool,
        session_changed: bool,
    ) {
        if !session_changed
            && Arc::ptr_eq(&messages, &self.transcript_messages)
            && generating == self.transcript_generating
        {
            return;
        }

        let old_message_count = self.transcript_messages.len();
        let old_row_count = self.transcript_rows.len();
        let new_message_count = messages.len();

        if !session_changed
            && new_message_count == old_message_count
            && generating == self.transcript_generating
        {
            let last_changed = messages
                .last()
                .zip(self.transcript_messages.last())
                .is_some_and(|(new, old)| {
                    new.id != old.id
                        || new.content.len() != old.content.len()
                        || new.reasoning_content.as_ref().map(String::len)
                            != old.reasoning_content.as_ref().map(String::len)
                        || new.tool_activities.len() != old.tool_activities.len()
                        || new.streaming != old.streaming
                });
            self.transcript_messages = messages;
            if last_changed {
                self.transcript_list_state
                    .remeasure_items(old_row_count.saturating_sub(1)..old_row_count);
            } else {
                self.transcript_list_state.remeasure();
            }
            return;
        }

        let new_rows = build_transcript_rows(&messages, generating);
        let new_row_count = new_rows.len();
        let working_changed = !session_changed
            && new_message_count == old_message_count
            && generating != self.transcript_generating;
        let prepended = !session_changed
            && new_message_count > old_message_count
            && self
                .transcript_messages
                .first()
                .zip(messages.get(new_message_count - old_message_count))
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .transcript_messages
                .last()
                .zip(messages.last())
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;
        let appended = !session_changed
            && new_message_count > old_message_count
            && self
                .transcript_messages
                .first()
                .zip(messages.first())
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .transcript_messages
                .last()
                .zip(messages.get(old_message_count.saturating_sub(1)))
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;

        self.transcript_messages = messages;
        self.transcript_rows = new_rows;
        self.transcript_generating = generating;
        if working_changed && generating {
            self.transcript_list_state
                .splice(old_row_count..old_row_count, 1);
        } else if working_changed {
            self.transcript_list_state
                .splice(new_row_count..old_row_count, 0);
        } else if prepended {
            self.transcript_list_state
                .splice(0..0, new_row_count - old_row_count);
        } else if appended {
            self.transcript_list_state
                .splice(old_row_count..old_row_count, new_row_count - old_row_count);
        } else {
            self.transcript_list_state.reset(new_row_count);
        }
        if session_changed {
            self.transcript_list_state.set_follow_mode(FollowMode::Tail);
        }
    }

    fn render_transcript_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let messages = Arc::clone(&self.transcript_messages);
        let content = match self.transcript_rows.get(index).cloned() {
            Some(TranscriptRow::Message(message_index)) => messages
                .get(message_index)
                .map(|message| self.render_message(message, cx)),
            Some(TranscriptRow::Activities(range)) => messages
                .get(range)
                .map(|messages| self.render_activity_group(messages, cx)),
            Some(TranscriptRow::Working) => Some(self.render_working_indicator(cx)),
            None => None,
        };

        div()
            .w_full()
            .max_w(px(CHAT_CONTENT_MAX_WIDTH))
            .mx_auto()
            .children(content)
            .into_any_element()
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
                Icon::new(if is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .on_click(move |_event, _window, cx| {
                model.update(cx, |state, cx| {
                    controller::dispatch(state, AppAction::ToggleReasoningExpanded(msg_id.clone()));
                    cx.notify();
                });
            });

        let detail = is_expanded.then(|| {
            let container = div()
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
                .overflow_y_scrollbar();
            if !should_use_markdown(is_streaming) {
                return container.child(reasoning.to_string()).into_any_element();
            }
            let entry = self
                .markdown_states
                .entry(format!("reasoning-{}", msg.id))
                .or_insert_with(|| {
                    let state = cx.new(|cx| TextViewState::markdown(reasoning, cx));
                    (reasoning.len(), state)
                });
            if entry.0 != reasoning.len() {
                entry.0 = reasoning.len();
                entry
                    .1
                    .update(cx, |state, cx| state.set_text(reasoning, cx));
            }
            container
                .child(TextView::new(&entry.1).selectable(true))
                .into_any_element()
        });

        Some(
            Collapsible::new()
                .open(is_expanded)
                .child(header)
                .when_some(detail, |c, content| c.content(content))
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
                        .max_w(px(USER_BUBBLE_MAX_WIDTH))
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
                                        (content.len(), state)
                                    });
                            if entry.0 != msg.content.len() {
                                entry.0 = msg.content.len();
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
                                    move |_event, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ));
                                        window.push_notification(
                                            Notification::info("Copied to clipboard"),
                                            cx,
                                        );
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
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(reasoning_element)
                            .children(if !msg.content.is_empty() {
                                let content_element =
                                    div().w_full().text_sm().text_color(theme.foreground);
                                let content_element = if !should_use_markdown(msg.streaming) {
                                    content_element
                                        .child(msg.content.clone())
                                        .into_any_element()
                                } else {
                                    let markdown = {
                                        let entry = self
                                            .markdown_states
                                            .entry(msg.id.clone())
                                            .or_insert_with(|| {
                                                let content = msg.content.clone();
                                                let state = cx.new(|cx| {
                                                    TextViewState::markdown(&content, cx)
                                                });
                                                (content.len(), state)
                                            });
                                        if entry.0 != msg.content.len() {
                                            entry.0 = msg.content.len();
                                            entry.1.update(cx, |state, cx| {
                                                state.set_text(&msg.content, cx);
                                            });
                                        }
                                        TextView::new(&entry.1).selectable(true)
                                    };
                                    content_element.child(markdown).into_any_element()
                                };

                                Some(if msg.streaming {
                                    div()
                                        .child(content_element)
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
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
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
                                move |_event, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    window.push_notification(
                                        Notification::info("Copied to clipboard"),
                                        cx,
                                    );
                                },
                            ))
                        }
                    }),
            ),
            MessageRole::Advisor(severity) => {
                let (badge_text, bg_color, border_color, text_color) = match severity {
                    threadlane_session::AdvisorSeverity::Aside => (
                        "ADVISOR ASIDE",
                        theme.secondary,
                        theme.border,
                        theme.secondary_foreground,
                    ),
                    threadlane_session::AdvisorSeverity::Concern => (
                        "ADVISOR CONCERN",
                        theme.warning,
                        theme.warning,
                        theme.warning_foreground,
                    ),
                    threadlane_session::AdvisorSeverity::Blocker => (
                        "ADVISOR BLOCKER",
                        theme.danger,
                        theme.danger,
                        theme.danger_foreground,
                    ),
                };

                div().w_full().flex().justify_center().my_2().px_4().child(
                    div()
                        .w_full()
                        .p_3()
                        .rounded_lg()
                        .bg(bg_color)
                        .border_1()
                        .border_color(border_color)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div().flex().items_center().gap_2().child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(text_color)
                                    .child(badge_text),
                            ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(text_color)
                                .child(msg.content.clone())
                                .context_menu({
                                    let content = msg.content.clone();
                                    move |menu, _window, _cx| {
                                        let text = content.clone();
                                        menu.item(PopupMenuItem::new("Copy Note").on_click(
                                            move |_event, window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text.clone(),
                                                ));
                                                window.push_notification(
                                                    Notification::info("Copied to clipboard"),
                                                    cx,
                                                );
                                            },
                                        ))
                                    }
                                }),
                        ),
                )
            }
            MessageRole::Error => div().flex().justify_center().my_2().px_4().child(
                div()
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
                                                move |_event, window, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(text.clone()),
                                                    );
                                                    window.push_notification(
                                                        Notification::info("Copied to clipboard"),
                                                        cx,
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
            .small()
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
                    .gap_1()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("What should we build in")
                    .child(project_picker)
                    .child("?"),
            )
            .into_any_element()
    }

    fn render_permission_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_permissions.get(session_id)?.clone();
        let theme = cx.theme().colors;

        let action_button = |id: &'static str,
                             label: &'static str,
                             decision: threadlane_session::PermissionDecision,
                             primary: bool,
                             danger: bool| {
            let model = self.model.clone();
            let request_id = request.id.clone();
            Button::new(id)
                .label(label)
                .xsmall()
                .when(primary, |button| button.primary())
                .when(danger, |button| button.danger())
                .on_click(move |_event, _window, cx| {
                    model.update(cx, |state, cx| {
                        state.resolve_active_permission(&request_id, decision);
                        cx.notify();
                    });
                })
        };

        Some(
            div()
                .w_full()
                .flex_none()
                .px_4()
                .pt_1()
                .bg(theme.background)
                .child(
                    div()
                        .w_full()
                        .max_w(px(1000.0))
                        .mx_auto()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(request.title),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child(request.detail),
                                ),
                        )
                        .child(action_button(
                            "permission-deny",
                            "Deny",
                            threadlane_session::PermissionDecision::Deny,
                            false,
                            true,
                        ))
                        .child(action_button(
                            "permission-allow-once",
                            "Allow once",
                            threadlane_session::PermissionDecision::AllowOnce,
                            false,
                            false,
                        ))
                        .child(action_button(
                            "permission-allow-always",
                            "Always",
                            threadlane_session::PermissionDecision::AllowAlways,
                            true,
                            false,
                        )),
                )
                .into_any_element(),
        )
    }

    /// Cached slash-command discovery. `available_slash_commands` scans
    /// extension directories and compiles each installed WASM module just to
    /// read its manifest, so it must not run per keystroke in render. The
    /// cache is keyed by project root and refreshed at most once per TTL
    /// while the command menu is open.
    fn cached_slash_commands(
        &mut self,
        project_root: Option<&std::path::Path>,
    ) -> Vec<SlashCommandInfo> {
        const SLASH_COMMAND_CACHE_TTL: Duration = Duration::from_secs(10);
        let project_root = project_root.map(std::path::Path::to_path_buf);
        if let Some((root, loaded_at, commands)) = &self.slash_command_cache {
            if *root == project_root && loaded_at.elapsed() < SLASH_COMMAND_CACHE_TTL {
                return commands.clone();
            }
        }
        let commands = available_slash_commands(project_root.as_deref());
        self.slash_command_cache =
            Some((project_root, std::time::Instant::now(), commands.clone()));
        commands
    }

    fn render_composer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let input_state = self.input_state.clone();
        let (selected_model, reasoning_effort, is_generating, pending_message, active_session_id) = {
            let state = self.model.read(cx);
            (
                state.selected_model.clone(),
                state.reasoning_effort,
                state.is_generating,
                state.active_pending_composer_message().map(str::to_owned),
                state.active_session_id.clone(),
            )
        };
        let metrics = self.model.read(cx).active_session_metrics();
        let lane_count = self
            .model
            .read(cx)
            .active_trajectory()
            .iter()
            .filter_map(|entry| entry.lane.as_deref())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let has_prompt =
            !self.input_state.read(cx).value().trim().is_empty() || !self.pasted_images.is_empty();
        let (model_options, selected_option, project_root) = {
            let state = self.model.read(cx);
            let options = state.available_models().to_vec();
            let opt = options.iter().find(|o| o.id == selected_model).cloned();
            let project = state.active_work_dir.clone();
            (options, opt, project)
        };
        let has_models = !model_options.is_empty();
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
                            .icon(Icon::default().path("icons/effort.svg"))
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
            let commands = self
                .cached_slash_commands(project_root.as_deref())
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

        let meter_color = if percent == 0.0 {
            theme.muted_foreground
        } else if percent >= 95.0 {
            theme.danger
        } else if percent >= 80.0 {
            theme.warning
        } else {
            theme.accent
        };
        let context_metrics = self.model.read(cx).active_session_metrics();
        let total_processed = context_metrics
            .billed_input_tokens()
            .saturating_add(context_metrics.output_tokens)
            .min(u64::from(u32::MAX)) as u32;
        let context_used = crate::model_catalog::format_tokens(token_usage.total_tokens);
        let context_limit = crate::model_catalog::format_tokens(context_max);
        let processed = crate::model_catalog::format_tokens(total_processed);
        let context_meter =
            HoverCard::new("context-window-hover-card")
                .anchor(Anchor::BottomRight)
                .open_delay(Duration::from_millis(200))
                .close_delay(Duration::from_millis(300))
                .trigger(
                    div()
                        .id("context-meter-badge")
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(32.0))
                        .rounded_full()
                        .hover(|style| style.bg(theme.accent.opacity(0.1)))
                        .cursor_pointer()
                        .child(
                            ProgressCircle::new("context-meter-circle")
                                .value(percent as f32)
                                .color(meter_color)
                                .size(px(24.0)),
                        ),
                )
                .content(move |_state, _window, _cx| {
                    let bar_width = ((percent / 100.0) * 308.0).clamp(0.0, 308.0);
                    div()
                        .w(px(340.0))
                        .p_4()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child("Context Window"),
                                )
                                .child(div().text_sm().text_color(theme.muted_foreground).child(
                                    format!("{percent:.0}% · {context_used}/{context_limit}"),
                                )),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(5.0))
                                .rounded_full()
                                .bg(theme.border)
                                .child(
                                    div()
                                        .h_full()
                                        .w(px(bar_width as f32))
                                        .rounded_full()
                                        .bg(meter_color),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("Total processed"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(processed.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Context is automatically compacted when needed."),
                        )
                });

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
                        .child(IconName::File)
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
                .icon(IconName::Folder)
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

        let billed_input_tokens = metrics.billed_input_tokens();
        let cache_hit = metrics
            .cache_hit_percent()
            .map(|percent| format!(" · Cache hit {percent}%"))
            .unwrap_or_default();

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
                            .child(div().flex_1())
                            .child(stash_button)
                            .child(context_meter)
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
                                            this.transcript_list_state.scroll_to_end();
                                            cx.notify();
                                        }
                                    })),
                            ),
                    ),
            )
            .when(
                metrics.turns > 0
                    || metrics.tool_calls > 0
                    || billed_input_tokens > 0
                    || metrics.output_tokens > 0
                    || lane_count > 0,
                |this| {
                    this.child(
                        div()
                            .w_full()
                            .max_w(px(1000.0))
                            .mx_auto()
                            .flex()
                            .justify_center()
                            .pt_1()
                            .pb_2()
                            .px_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} turns · {} tool calls{cache_hit} · {} input / {} output tokens · {} subagent lanes",
                                metrics.turns,
                                metrics.tool_calls,
                                crate::model_catalog::format_tokens(
                                    billed_input_tokens.min(u64::from(u32::MAX)) as u32
                                ),
                                crate::model_catalog::format_tokens(
                                    metrics.output_tokens.min(u64::from(u32::MAX)) as u32
                                ),
                                lane_count,
                            )),
                    )
                },
            )
    }
}

impl Render for ChatListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (messages, is_new_task, active_plan, session_key, has_older, is_generating) = {
            let state = self.model.read(cx);
            (
                state.messages.clone(),
                state.is_new_task,
                state.active_plan.clone(),
                state
                    .active_work_dir
                    .clone()
                    .zip(state.active_session_id.clone()),
                state.has_older_messages(),
                state.is_generating,
            )
        };
        let session_changed = session_key != self.last_session_key;
        if session_changed {
            self.last_session_key = session_key;
            self.initial_scroll_frames = 6;
            self.trajectory_category = None;
            self.trajectory_lane = None;
            self.selected_trajectory_index = None;
            self.trajectory_search.clear();
            self.markdown_states.clear();
            self.trajectory_cache = None;
            self.trajectory_search_input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }
        self.sync_transcript_rows(messages.clone(), is_generating, session_changed);
        if let Some(prompt) = self
            .model
            .update(cx, |state, _cx| state.requested_composer_prompt.take())
        {
            self.current_tab = CentralTab::Chat;
            self.input_state.update(cx, |input, cx| {
                input.set_value(&prompt, window, cx);
            });
        }
        if self.initial_scroll_frames > 0 {
            self.transcript_list_state.scroll_to_end();
            self.initial_scroll_frames = self.initial_scroll_frames.saturating_sub(1);
        }
        if has_older
            && self.initial_scroll_frames == 0
            && !self.older_load_pending
            && self.transcript_list_state.logical_scroll_top().item_ix <= 1
        {
            self.older_load_pending = true;
            let model = self.model.clone();
            cx.spawn(async move |this, cx| {
                let request = model.update(cx, |state, _cx| state.history_page_request());
                if let Some((session_file, cursor, page_serial)) = request {
                    let page_file = session_file.clone();
                    let page = cx
                        .background_executor()
                        .spawn(async move {
                            compute_message_page(&page_file, Some(cursor), page_serial)
                        })
                        .await;
                    model.update(cx, |state, cx| {
                        let added = page
                            .map(|page| state.apply_older_message_page(&session_file, page))
                            .unwrap_or_default();
                        if added > 0 {
                            cx.notify();
                        }
                    });
                }
                let _ = this.update(cx, |view, cx| {
                    view.older_load_pending = false;
                    cx.notify();
                });
            })
            .detach();
        }
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
            .child(match self.current_tab {
                CentralTab::Editor => self.editor.clone().into_any_element(),
                CentralTab::Trajectory => self.render_trajectory(cx),
                CentralTab::Chat => {
                    if is_new_task {
                        self.render_new_task(cx)
                    } else if messages.is_empty() {
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                "No messages in this session yet. Type a prompt below to begin.",
                            ))
                            .into_any_element()
                    } else {
                        div()
                            .id("chat-transcript-container")
                            .relative()
                            .w_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                list(
                                    self.transcript_list_state.clone(),
                                    cx.processor(Self::render_transcript_row),
                                )
                                .size_full()
                                .pt_3()
                                .pb_6()
                                .with_sizing_behavior(ListSizingBehavior::Auto),
                            )
                            .child(div().absolute().inset_0().child(
                                gpui_component::scroll::Scrollbar::vertical(
                                    &self.transcript_list_state,
                                ),
                            ))
                            .into_any_element()
                    }
                }
            })
            .children(
                (self.current_tab == CentralTab::Chat)
                    .then(|| self.render_plan_tracker(&active_plan, cx))
                    .flatten(),
            )
            .children(
                (self.current_tab == CentralTab::Chat)
                    .then(|| self.render_permission_prompt(cx))
                    .flatten(),
            )
            .children((self.current_tab == CentralTab::Chat).then(|| self.render_composer(cx)))
    }
}

#[cfg(test)]
mod hot_path_tests {
    use super::{
        build_transcript_rows, format_trajectory_raw_json, grouped_tool_activities,
        should_use_markdown, TrajectoryCacheKey, TrajectoryMode, TranscriptRow,
    };
    use crate::state::{
        ChatMessageInfo, MessageRole, ToolActivityInfo, TrajectoryDiagnostics, TrajectoryEntry,
    };

    #[test]
    fn markdown_is_deferred_until_streaming_completes() {
        assert!(!should_use_markdown(true));
        assert!(should_use_markdown(false));
    }

    #[test]
    fn grouped_tool_activities_borrows_in_order_and_hides_plan_updates() {
        let activity_message = |activities: &[(&str, &str)]| ChatMessageInfo {
            id: activities[0].0.into(),
            role: MessageRole::Assistant,
            content: String::new(),
            tool_activities: activities
                .iter()
                .map(|(id, title)| ToolActivityInfo {
                    id: (*id).into(),
                    category: "tool".into(),
                    title: (*title).into(),
                    summary: String::new(),
                    display_summary: String::new(),
                    detail: String::new(),
                    is_expanded: false,
                })
                .collect(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        };
        let messages = vec![
            activity_message(&[("tool-1", "read_file"), ("plan", "update_plan")]),
            activity_message(&[("tool-2", "write_file")]),
        ];

        let ids = grouped_tool_activities(&messages)
            .map(|activity| activity.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["tool-1", "tool-2"]);
    }

    #[test]
    fn transcript_rows_group_consecutive_tool_only_messages() {
        let message = |id: &str, activity: bool| ChatMessageInfo {
            id: id.into(),
            role: if activity {
                MessageRole::Assistant
            } else {
                MessageRole::User
            },
            content: if activity { "" } else { id }.into(),
            tool_activities: activity
                .then(|| ToolActivityInfo {
                    id: format!("tool-{id}"),
                    category: "tool".into(),
                    title: "read_file".into(),
                    summary: String::new(),
                    display_summary: String::new(),
                    detail: String::new(),
                    is_expanded: false,
                })
                .into_iter()
                .collect(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        };
        let messages = vec![
            message("user", false),
            message("tool-1", true),
            message("tool-2", true),
            message("answer", false),
        ];

        assert_eq!(
            build_transcript_rows(&messages, true),
            vec![
                TranscriptRow::Message(0),
                TranscriptRow::Activities(1..3),
                TranscriptRow::Message(3),
                TranscriptRow::Working,
            ]
        );
    }


    #[test]
    fn trajectory_raw_json_is_prepared_for_the_revision_cache() {
        let entry = TrajectoryEntry {
            seq: Some(1),
            run_id: None,
            turn: None,
            request: None,
            category: "Tool".into(),
            summary: "Read file".into(),
            detail: "src/main.rs".into(),
            lane: None,
            correlation_id: None,
            diagnostics: TrajectoryDiagnostics::default(),
        };

        let raw = format_trajectory_raw_json(&entry);

        assert!(raw.contains("\"category\": \"Tool\""));
        assert!(raw.contains("\"summary\": \"Read file\""));
    }
    #[test]
    fn trajectory_cache_key_changes_with_data_or_filter() {
        let base = TrajectoryCacheKey {
            revision: 7,
            mode: TrajectoryMode::Execution,
            query: "tool".into(),
            category: None,
            lane: None,
        };
        let mut changed = base.clone();
        changed.revision += 1;
        assert_ne!(base, changed);

        let mut changed = base.clone();
        changed.query = "provider".into();
        assert_ne!(base, changed);
    }
}
