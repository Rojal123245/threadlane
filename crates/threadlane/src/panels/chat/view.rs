//! Chat panel main view & transcript list widget.

use super::state::{ChatMessage, MsgRole, StreamingKind, ToolIcon, ToolStatus};
use crate::components::tool_fold_header::ToolFoldHeaderAction;
use crate::path_utils::{compact_workspace_path, truncate_chars};
use crate::workspace::AppState;
use makepad_widgets::*;

const TOOL_ICON_MAP: [(ToolIcon, &[LiveId; 1]); 8] = [
    (ToolIcon::Generic, ids!(icon_generic)),
    (ToolIcon::ReadFile, ids!(icon_read_file)),
    (ToolIcon::WriteFile, ids!(icon_write_file)),
    (ToolIcon::EditFile, ids!(icon_edit_file)),
    (ToolIcon::ListDirectory, ids!(icon_list_directory)),
    (ToolIcon::Terminal, ids!(icon_terminal)),
    (ToolIcon::Skill, ids!(icon_skill)),
    (ToolIcon::Subagent, ids!(icon_subagent)),
];

fn show_tool_icon(cx: &mut Cx, item: &WidgetRef, selected: ToolIcon) {
    for (icon, id) in TOOL_ICON_MAP {
        item.widget(cx, id).set_visible(cx, selected == icon);
    }
}

fn update_activity_status(
    cx: &mut Cx,
    item_widget: &WidgetRef,
    running: bool,
    error: bool,
    cancelled: bool,
) {
    let indicator = item_widget.widget(cx, ids!(status_indicator));
    indicator
        .widget(cx, ids!(status_running_indicator))
        .set_visible(cx, running);
    indicator
        .widget(cx, ids!(status_done_indicator))
        .set_visible(cx, !running && !error && !cancelled);
    indicator
        .widget(cx, ids!(status_cancelled_indicator))
        .set_visible(cx, !running && !error && cancelled);
    indicator
        .widget(cx, ids!(status_error_lbl))
        .set_visible(cx, !running && error);
}

#[derive(Clone, Copy)]
enum DisplayRow {
    Message(usize),
    ActivityGroup {
        start: usize,
        end: usize,
        streaming_thinking: bool,
    },
    StreamingAssistant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    ExploredFile,
    ExploredFolder,
    Search,
    Edited,
    Command,
    Skill,
    Delegated,
    Other,
}

#[derive(Default)]
struct ActivityCounts {
    explored_files: usize,
    explored_folders: usize,
    searches: usize,
    edited: usize,
    commands: usize,
    skills: usize,
    delegated: usize,
    other: usize,
}

impl ActivityCounts {
    fn add(&mut self, kind: ActivityKind) {
        match kind {
            ActivityKind::ExploredFile => self.explored_files += 1,
            ActivityKind::ExploredFolder => self.explored_folders += 1,
            ActivityKind::Search => self.searches += 1,
            ActivityKind::Edited => self.edited += 1,
            ActivityKind::Command => self.commands += 1,
            ActivityKind::Skill => self.skills += 1,
            ActivityKind::Delegated => self.delegated += 1,
            ActivityKind::Other => self.other += 1,
        }
    }
}

fn is_activity(message: &ChatMessage) -> bool {
    matches!(
        message,
        ChatMessage::Thinking { .. } | ChatMessage::Tool { .. }
    )
}

fn display_rows(
    messages: &[ChatMessage],
    streaming_kind: Option<StreamingKind>,
    streaming_text: &str,
) -> Vec<DisplayRow> {
    let mut rows = Vec::new();

    for (message_index, message) in messages.iter().enumerate() {
        if is_activity(message) {
            if let Some(DisplayRow::ActivityGroup { end, .. }) = rows.last_mut() {
                if *end == message_index {
                    *end = message_index + 1;
                    continue;
                }
            }
            rows.push(DisplayRow::ActivityGroup {
                start: message_index,
                end: message_index + 1,
                streaming_thinking: false,
            });
        } else {
            rows.push(DisplayRow::Message(message_index));
        }
    }

    if !streaming_text.is_empty() {
        match streaming_kind {
            Some(StreamingKind::Thinking) => {
                if let Some(DisplayRow::ActivityGroup {
                    end,
                    streaming_thinking,
                    ..
                }) = rows.last_mut()
                {
                    if *end == messages.len() {
                        *streaming_thinking = true;
                    } else {
                        rows.push(DisplayRow::ActivityGroup {
                            start: messages.len(),
                            end: messages.len(),
                            streaming_thinking: true,
                        });
                    }
                } else {
                    rows.push(DisplayRow::ActivityGroup {
                        start: messages.len(),
                        end: messages.len(),
                        streaming_thinking: true,
                    });
                }
            }
            _ => rows.push(DisplayRow::StreamingAssistant),
        }
    }

    rows
}

fn activity_kind(name: &str, icon: ToolIcon) -> ActivityKind {
    let normalized = name.to_ascii_lowercase();
    if icon == ToolIcon::ListDirectory || normalized.contains("list") {
        ActivityKind::ExploredFolder
    } else if normalized.contains("search")
        || normalized.contains("grep")
        || normalized.contains("find")
    {
        ActivityKind::Search
    } else if icon == ToolIcon::ReadFile || normalized.contains("read") {
        ActivityKind::ExploredFile
    } else if matches!(icon, ToolIcon::WriteFile | ToolIcon::EditFile)
        || normalized.contains("write")
        || normalized.contains("edit")
    {
        ActivityKind::Edited
    } else if icon == ToolIcon::Terminal
        || normalized.contains("command")
        || normalized.contains("terminal")
        || normalized.contains("shell")
    {
        ActivityKind::Command
    } else if icon == ToolIcon::Skill || normalized.contains("skill") {
        ActivityKind::Skill
    } else if icon == ToolIcon::Subagent
        || normalized.contains("subagent")
        || normalized.contains("delegate")
    {
        ActivityKind::Delegated
    } else {
        ActivityKind::Other
    }
}

fn pluralized(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn activity_preview(counts: &ActivityCounts, has_thinking: bool) -> String {
    let mut parts = Vec::new();
    if has_thinking {
        parts.push("Reasoned".to_string());
    }
    let mut explored = Vec::new();
    if counts.explored_files > 0 {
        explored.push(pluralized(counts.explored_files, "file", "files"));
    }
    if counts.explored_folders > 0 {
        explored.push(pluralized(counts.explored_folders, "folder", "folders"));
    }
    if counts.searches > 0 {
        explored.push(pluralized(counts.searches, "search", "searches"));
    }
    if !explored.is_empty() {
        parts.push(format!("Explored {}", explored.join(", ")));
    }
    if counts.edited > 0 {
        parts.push(format!(
            "Edited {}",
            pluralized(counts.edited, "file", "files")
        ));
    }
    if counts.commands > 0 {
        parts.push(format!(
            "Ran {}",
            pluralized(counts.commands, "command", "commands")
        ));
    }
    if counts.skills > 0 {
        parts.push(format!(
            "Loaded {}",
            pluralized(counts.skills, "skill", "skills")
        ));
    }
    if counts.delegated > 0 {
        parts.push(format!(
            "Delegated {}",
            pluralized(counts.delegated, "task", "tasks")
        ));
    }
    if counts.other > 0 {
        parts.push(format!(
            "Used {}",
            pluralized(counts.other, "tool", "tools")
        ));
    }
    parts.join(" · ")
}

fn markdown_inline(text: &str) -> String {
    text.replace(['\r', '\n'], " ").replace('`', "'")
}

fn activity_line(
    kind: ActivityKind,
    title: &str,
    primary: &str,
    result_metadata: &str,
    status: ToolStatus,
) -> String {
    let action = match kind {
        ActivityKind::ExploredFile | ActivityKind::ExploredFolder | ActivityKind::Search => {
            "Explored"
        }
        ActivityKind::Edited => "Edited",
        ActivityKind::Command => "Ran command",
        ActivityKind::Skill => "Loaded skill",
        ActivityKind::Delegated => "Delegated",
        ActivityKind::Other => title,
    };
    let mut line = format!("- **{}**", markdown_inline(action));
    if !primary.is_empty() {
        line.push_str(&format!(" `{}`", markdown_inline(primary)));
    }
    match status {
        ToolStatus::Running => line.push_str(" · Running"),
        ToolStatus::Error => line.push_str(" · Failed"),
        ToolStatus::Cancelled if !result_metadata.is_empty() => {
            line.push_str(&format!(" · {}", markdown_inline(result_metadata)))
        }
        ToolStatus::Cancelled => line.push_str(" · Stopped"),
        ToolStatus::Done if !result_metadata.is_empty() => {
            line.push_str(&format!(" · {}", markdown_inline(result_metadata)))
        }
        ToolStatus::Done => {}
    }
    line
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityDetailKind {
    Thinking,
    Tool,
}

fn append_activity_detail(
    detail: &mut String,
    previous_kind: &mut Option<ActivityDetailKind>,
    kind: ActivityDetailKind,
    block: &str,
) {
    if block.is_empty() {
        return;
    }
    if !detail.is_empty() {
        if *previous_kind == Some(ActivityDetailKind::Tool) && kind == ActivityDetailKind::Tool {
            detail.push('\n');
        } else {
            detail.push_str("\n\n");
        }
    }
    detail.push_str(block);
    *previous_kind = Some(kind);
}

fn activity_detail(messages: &[ChatMessage], streaming_thinking: Option<&str>) -> String {
    let mut detail = String::new();
    let mut previous_kind = None;
    let mut has_thinking = false;

    for message in messages {
        match message {
            ChatMessage::Thinking { text } => {
                has_thinking = true;
                if !text.trim().is_empty() {
                    append_activity_detail(
                        &mut detail,
                        &mut previous_kind,
                        ActivityDetailKind::Thinking,
                        &format!("**Thinking**\n\n{text}"),
                    );
                }
            }
            ChatMessage::Tool {
                name,
                status,
                presentation,
                result_metadata,
                output,
                ..
            } => {
                let kind = activity_kind(name, presentation.icon);
                let mut line = activity_line(
                    kind,
                    &presentation.title,
                    &presentation.primary,
                    result_metadata,
                    *status,
                );
                if name == "subagent" || presentation.icon == ToolIcon::Subagent {
                    if !presentation.arguments_detail.is_empty() {
                        line.push_str("\n\n");
                        line.push_str(&presentation.arguments_detail);
                    }
                    if !output.trim().is_empty() {
                        line.push_str("\n\n");
                        line.push_str(output.trim());
                    }
                }
                append_activity_detail(
                    &mut detail,
                    &mut previous_kind,
                    ActivityDetailKind::Tool,
                    &line,
                );
            }
            ChatMessage::Text { .. } => {}
        }
    }

    if let Some(text) = streaming_thinking {
        has_thinking = true;
        let block = if text.trim().is_empty() {
            "**Thinking…**".to_string()
        } else {
            format!("**Thinking…**\n\n{text}")
        };
        append_activity_detail(
            &mut detail,
            &mut previous_kind,
            ActivityDetailKind::Thinking,
            &block,
        );
    }

    if detail.is_empty() && has_thinking {
        "Reasoning completed.".to_string()
    } else {
        detail
    }
}

fn user_message_needs_wrapping(text: &str) -> bool {
    const COMPACT_LINE_CHAR_LIMIT: usize = 88;

    text.lines()
        .any(|line| line.chars().count() > COMPACT_LINE_CHAR_LIMIT)
}

fn draw_markdown_item(
    list: &mut PortalList,
    cx: &mut Cx2d,
    item_id: usize,
    template: LiveId,
    text: &str,
) {
    let item_widget = list.item(cx, item_id, template);
    item_widget.markdown(cx, ids!(md)).set_text(cx, text);
    item_widget.draw_all_unscoped(cx);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StarterPromptAction {
    Explore,
    Build,
    Review,
    Fix,
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ChatList {
    #[deref]
    view: View,
    /// Cached display rows; rebuilt only when message count or streaming kind changes.
    #[rust]
    cached_rows: Vec<DisplayRow>,
    #[rust]
    cached_msg_count: usize,
    #[rust]
    cached_streaming_kind: Option<StreamingKind>,
    #[rust]
    cached_streaming_text_len: usize,
    #[rust]
    hovered_starter: Option<StarterPromptAction>,
    #[rust]
    pressed_starter: Option<StarterPromptAction>,
}

impl Widget for ChatList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(data) = scope
            .data
            .get::<AppState>()
            .and_then(AppState::active_workspace)
            .map(|workspace| workspace.chat.clone())
        else {
            return DrawStep::done();
        };

        // Rebuild display rows only when the message list or streaming state changes.
        let msg_count = data.messages.len();
        let streaming_text_len = data.streaming_text.len();
        if msg_count != self.cached_msg_count
            || data.streaming_kind != self.cached_streaming_kind
            || streaming_text_len != self.cached_streaming_text_len
        {
            self.cached_rows =
                display_rows(&data.messages, data.streaming_kind, &data.streaming_text);
            self.cached_msg_count = msg_count;
            self.cached_streaming_kind = data.streaming_kind;
            self.cached_streaming_text_len = streaming_text_len;
        }
        let rows = &self.cached_rows;

        let is_empty = data.messages.is_empty() && data.streaming_text.is_empty();

        // Toggle the empty-state overlay — it lives as a sibling to the PortalList
        // so it can use height: Fill and truly center its content.
        let empty_state = self.view.widget(cx, ids!(empty_state));
        if is_empty {
            if let Some(key) = scope.data.get::<AppState>().and_then(|s| s.active_key()) {
                let name = key
                    .work_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| key.work_dir.display().to_string());
                let name = truncate_chars(&name, 40);
                empty_state
                    .label(cx, ids!(project_name_inline_lbl))
                    .set_text(cx, &name);
                let home_dir = std::env::var_os("HOME").map(std::path::PathBuf::from);
                let path = compact_workspace_path(&key.work_dir, home_dir.as_deref());
                empty_state
                    .label(cx, ids!(workspace_path_lbl))
                    .set_text(cx, &path);
            }
        }
        empty_state.set_visible(cx, is_empty);
        // The PortalList is the later sibling in this overlay and otherwise sits above the
        // welcome cards, intercepting their pointer events even when it has no rows.
        self.view.widget(cx, ids!(list)).set_visible(cx, !is_empty);

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, rows.len());

                while let Some(item_id) = list.next_visible_item(cx) {
                    let Some(row) = rows.get(item_id).copied() else {
                        continue;
                    };

                    match row {
                        DisplayRow::StreamingAssistant => {
                            draw_markdown_item(
                                &mut list,
                                cx,
                                item_id,
                                id!(AssistantMsg),
                                &data.streaming_text,
                            );
                        }
                        DisplayRow::ActivityGroup {
                            start,
                            end,
                            streaming_thinking,
                        } => {
                            let item_widget = list.item(cx, item_id, id!(ActivityGroupMsg));
                            let mut counts = ActivityCounts::default();
                            let mut has_thinking = streaming_thinking;
                            let mut running = streaming_thinking;
                            let mut has_error = false;
                            let mut has_cancelled = false;
                            let mut first_icon = None;
                            let mut mixed_icons = false;

                            for message in &data.messages[start..end] {
                                match message {
                                    ChatMessage::Thinking { .. } => has_thinking = true,
                                    ChatMessage::Tool {
                                        name,
                                        status,
                                        presentation,
                                        ..
                                    } => {
                                        let kind = activity_kind(name, presentation.icon);
                                        counts.add(kind);
                                        running |= *status == ToolStatus::Running;
                                        has_error |= *status == ToolStatus::Error;
                                        has_cancelled |= *status == ToolStatus::Cancelled;
                                        if let Some(icon) = first_icon {
                                            mixed_icons |= icon != presentation.icon;
                                        } else {
                                            first_icon = Some(presentation.icon);
                                        }
                                    }
                                    ChatMessage::Text { .. } => {}
                                }
                            }

                            let detail = activity_detail(
                                &data.messages[start..end],
                                streaming_thinking.then_some(data.streaming_text.as_str()),
                            );

                            show_tool_icon(
                                cx,
                                &item_widget,
                                if mixed_icons {
                                    ToolIcon::Generic
                                } else {
                                    first_icon.unwrap_or(ToolIcon::Generic)
                                },
                            );
                            let title = if running {
                                "Working"
                            } else if has_cancelled {
                                "Stopped"
                            } else {
                                "Worked"
                            };
                            item_widget.label(cx, ids!(title_lbl)).set_text(cx, title);
                            item_widget
                                .label(cx, ids!(preview_lbl))
                                .set_text(cx, &activity_preview(&counts, has_thinking));
                            update_activity_status(
                                cx,
                                &item_widget,
                                running,
                                has_error,
                                has_cancelled,
                            );
                            item_widget.markdown(cx, ids!(md)).set_text(cx, &detail);
                            item_widget.draw_all_unscoped(cx);
                        }
                        DisplayRow::Message(message_index) => {
                            let Some(message) = data.messages.get(message_index) else {
                                continue;
                            };
                            match message {
                                ChatMessage::Text { role, text } => match role {
                                    MsgRole::User => {
                                        let template = if user_message_needs_wrapping(text) {
                                            id!(UserMsgWrapped)
                                        } else {
                                            id!(UserMsg)
                                        };
                                        draw_markdown_item(&mut list, cx, item_id, template, text);
                                    }
                                    MsgRole::Assistant => {
                                        draw_markdown_item(
                                            &mut list,
                                            cx,
                                            item_id,
                                            id!(AssistantMsg),
                                            text,
                                        );
                                    }
                                    MsgRole::System => {
                                        let item_widget = list.item(cx, item_id, id!(SystemMsg));
                                        item_widget.label(cx, ids!(lbl)).set_text(cx, text);
                                        item_widget.draw_all_unscoped(cx);
                                    }
                                },
                                ChatMessage::Thinking { text } => {
                                    let item_widget = list.item(cx, item_id, id!(ThinkingMsg));
                                    item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                    item_widget.label(cx, ids!(preview_lbl)).set_text(
                                        cx,
                                        &super::state::collapsed_thinking_preview(text, 72),
                                    );
                                    item_widget.draw_all_unscoped(cx);
                                }
                                ChatMessage::Tool {
                                    output,
                                    status,
                                    presentation,
                                    result_preview,
                                    result_metadata,
                                    ..
                                } => {
                                    let item_widget = list.item(cx, item_id, id!(ToolMsg));
                                    show_tool_icon(cx, &item_widget, presentation.icon);
                                    item_widget
                                        .label(cx, ids!(title_lbl))
                                        .set_text(cx, &presentation.title);
                                    item_widget
                                        .label(cx, ids!(meta_lbl))
                                        .set_text(cx, &presentation.metadata);
                                    item_widget
                                        .widget(cx, ids!(meta_lbl))
                                        .set_visible(cx, !presentation.metadata.is_empty());
                                    item_widget
                                        .label(cx, ids!(preview_lbl))
                                        .set_text(cx, &presentation.primary);
                                    item_widget
                                        .label(cx, ids!(result_meta_lbl))
                                        .set_text(cx, result_metadata);
                                    item_widget
                                        .widget(cx, ids!(result_meta_lbl))
                                        .set_visible(cx, !result_metadata.is_empty());

                                    let has_completed_result = *status != ToolStatus::Running;
                                    item_widget
                                        .label(cx, ids!(result_preview_lbl))
                                        .set_text(cx, result_preview);
                                    item_widget
                                        .widget(cx, ids!(result_preview_lbl))
                                        .set_visible(
                                            cx,
                                            has_completed_result && !result_preview.is_empty(),
                                        );
                                    item_widget
                                        .label(cx, ids!(result_meta_header_lbl))
                                        .set_text(cx, result_metadata);
                                    item_widget
                                        .widget(cx, ids!(result_meta_header_lbl))
                                        .set_visible(
                                            cx,
                                            has_completed_result && !result_metadata.is_empty(),
                                        );
                                    update_activity_status(
                                        cx,
                                        &item_widget,
                                        *status == ToolStatus::Running,
                                        *status == ToolStatus::Error,
                                        *status == ToolStatus::Cancelled,
                                    );
                                    item_widget
                                        .widget(cx, ids!(args_section))
                                        .label(cx, ids!(content_lbl))
                                        .set_text(cx, &presentation.arguments_detail);
                                    let arguments_are_fully_summarized = matches!(
                                        presentation.icon,
                                        ToolIcon::ReadFile
                                            | ToolIcon::ListDirectory
                                            | ToolIcon::Skill
                                    );
                                    item_widget.widget(cx, ids!(args_section)).set_visible(
                                        cx,
                                        !arguments_are_fully_summarized
                                            && !presentation.arguments_detail.is_empty(),
                                    );
                                    let output_detail =
                                        super::state::tool_result_detail(output, 6_000);
                                    let result_section =
                                        item_widget.widget(cx, ids!(result_section));
                                    result_section
                                        .label(cx, ids!(content_lbl))
                                        .set_text(cx, &output_detail);
                                    result_section
                                        .widget(cx, ids!(content_lbl))
                                        .set_visible(cx, !presentation.output_markdown);
                                    let content_md_wrap =
                                        result_section.widget(cx, ids!(content_md_wrap));
                                    content_md_wrap
                                        .markdown(cx, ids!(content_md))
                                        .set_text(cx, &output_detail);
                                    content_md_wrap.set_visible(cx, presentation.output_markdown);
                                    result_section.set_visible(cx, !output.is_empty());
                                    item_widget.draw_all_unscoped(cx);
                                }
                            }
                        }
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event {
            Event::MouseMove(mouse_event) => {
                let hovered = self.starter_prompt_at(cx, mouse_event.abs);
                self.set_starter_feedback(cx, hovered, self.pressed_starter);
            }
            Event::MouseDown(mouse_event) if mouse_event.button.is_primary() => {
                let pressed = self.starter_prompt_at(cx, mouse_event.abs);
                self.set_starter_feedback(cx, pressed, pressed);
            }
            Event::MouseUp(mouse_event) if mouse_event.button.is_primary() => {
                let hovered = self.starter_prompt_at(cx, mouse_event.abs);
                self.set_starter_feedback(cx, hovered, None);
            }
            Event::MouseLeave(_) => self.set_starter_feedback(cx, None, None),
            _ => {}
        }
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            let list = self.view.portal_list(cx, ids!(list));
            let layout_changed = list
                .items_with_actions(actions)
                .into_iter()
                .any(|(_, item)| {
                    actions
                        .find_widget_action(item.widget_uid())
                        .is_some_and(|action| {
                            matches!(
                                action.cast::<ToolFoldHeaderAction>(),
                                ToolFoldHeaderAction::LayoutChanged
                            )
                        })
                });
            if layout_changed {
                list.redraw(cx);
            }
        }

        if matches!(event, Event::KeyDown(key_event) if matches!(key_event.key_code, KeyCode::ReturnKey | KeyCode::Space))
        {
            if let Some(action) = self.focused_starter_action(cx) {
                cx.action(action);
            }
        }
    }
}

impl ChatList {
    fn focused_starter_action(&self, cx: &Cx) -> Option<StarterPromptAction> {
        [
            (ids!(explore_btn), StarterPromptAction::Explore),
            (ids!(build_btn), StarterPromptAction::Build),
            (ids!(review_btn), StarterPromptAction::Review),
            (ids!(fix_btn), StarterPromptAction::Fix),
        ]
        .into_iter()
        .find_map(|(path, action)| {
            cx.has_key_focus(self.view.widget(cx, path).area())
                .then_some(action)
        })
    }

    fn set_starter_feedback(
        &mut self,
        cx: &mut Cx,
        hovered: Option<StarterPromptAction>,
        pressed: Option<StarterPromptAction>,
    ) {
        if self.hovered_starter == hovered && self.pressed_starter == pressed {
            return;
        }
        self.hovered_starter = hovered;
        self.pressed_starter = pressed;

        for (path, action) in [
            (
                ids!(empty_state.cards_row.explore_card),
                StarterPromptAction::Explore,
            ),
            (
                ids!(empty_state.cards_row.build_card),
                StarterPromptAction::Build,
            ),
            (
                ids!(empty_state.cards_row.review_card),
                StarterPromptAction::Review,
            ),
            (
                ids!(empty_state.cards_row.fix_card),
                StarterPromptAction::Fix,
            ),
        ] {
            let (color, border_color) = if pressed == Some(action) {
                (
                    vec4(0.145, 0.188, 0.247, 1.0),
                    vec4(0.337, 0.463, 0.624, 1.0),
                )
            } else if hovered == Some(action) {
                (
                    vec4(0.129, 0.165, 0.212, 1.0),
                    vec4(0.247, 0.322, 0.412, 1.0),
                )
            } else {
                (
                    vec4(0.114, 0.137, 0.173, 1.0),
                    vec4(0.165, 0.204, 0.255, 1.0),
                )
            };
            let mut card = self.view.widget(cx, path);
            script_apply_eval!(cx, card, {
                draw_bg +: {
                    color: #(color)
                    border_color: #(border_color)
                }
            });
            card.redraw(cx);
        }
    }

    fn starter_prompt_at(&self, cx: &Cx, position: Vec2d) -> Option<StarterPromptAction> {
        if !self.view.widget(cx, ids!(empty_state)).visible() {
            return None;
        }
        let cards = [
            (
                ids!(empty_state.cards_row.explore_card),
                StarterPromptAction::Explore,
            ),
            (
                ids!(empty_state.cards_row.build_card),
                StarterPromptAction::Build,
            ),
            (
                ids!(empty_state.cards_row.review_card),
                StarterPromptAction::Review,
            ),
            (
                ids!(empty_state.cards_row.fix_card),
                StarterPromptAction::Fix,
            ),
        ];
        cards.into_iter().find_map(|(path, action)| {
            self.view
                .widget(cx, path)
                .area()
                .rect(cx)
                .contains(position)
                .then_some(action)
        })
    }
}

impl ChatListRef {
    pub fn starter_prompt_at(&self, cx: &Cx, position: Vec2d) -> Option<StarterPromptAction> {
        self.borrow()
            .and_then(|inner| inner.starter_prompt_at(cx, position))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn tool(id: &str, name: &str, arguments: &str) -> ChatMessage {
        ChatMessage::Tool {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
            output: String::new(),
            status: ToolStatus::Done,
            presentation: super::super::state::tool_presentation(name, arguments),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        }
    }

    #[test]
    fn long_user_lines_use_the_wrapped_message_layout() {
        assert!(!user_message_needs_wrapping("A short user message"));
        assert!(!user_message_needs_wrapping(
            "Several short lines\nstill stay compact"
        ));
        assert!(user_message_needs_wrapping(&"word ".repeat(90)));
    }

    #[test]
    fn consecutive_activity_messages_share_one_display_row() {
        let messages = vec![
            ChatMessage::Thinking {
                text: "Plan".into(),
            },
            tool("read", "read_file", r#"{"path":"src/app.rs"}"#),
            tool("edit", "edit_file", r#"{"path":"src/app.rs","edits":[]}"#),
            ChatMessage::Text {
                role: MsgRole::Assistant,
                text: "Done".into(),
            },
        ];

        let rows = display_rows(&messages, None, "");
        assert_eq!(rows.len(), 2);
        assert!(matches!(
            rows[0],
            DisplayRow::ActivityGroup {
                start: 0,
                end: 3,
                streaming_thinking: false
            }
        ));
        assert!(matches!(rows[1], DisplayRow::Message(3)));
    }

    #[test]
    fn streaming_thinking_merges_into_trailing_activity_group() {
        let messages = vec![tool("read", "read_file", r#"{"path":"src/app.rs"}"#)];

        let rows = display_rows(&messages, Some(StreamingKind::Thinking), "Reviewing");
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            DisplayRow::ActivityGroup {
                start: 0,
                end: 1,
                streaming_thinking: true
            }
        ));
    }

    #[test]
    fn activity_detail_preserves_finalized_and_streaming_thinking_in_order() {
        let completed = format!(
            "Starting analysis. {}Final persisted reasoning sentence.",
            "Detailed reasoning step. ".repeat(400)
        );
        let messages = vec![
            ChatMessage::Thinking {
                text: completed.clone(),
            },
            tool("read", "read_file", r#"{"path":"src/app.rs"}"#),
            ChatMessage::Thinking {
                text: "Reasoning after the tool.".into(),
            },
        ];

        let detail = activity_detail(&messages, Some("Current streaming reasoning."));

        assert!(detail.contains(&completed));
        let completed_index = detail.find("Final persisted reasoning sentence.").unwrap();
        let tool_index = detail.find("src/app.rs").unwrap();
        let resumed_index = detail.find("Reasoning after the tool.").unwrap();
        let streaming_index = detail.find("Current streaming reasoning.").unwrap();
        assert!(completed_index < tool_index);
        assert!(tool_index < resumed_index);
        assert!(resumed_index < streaming_index);
    }

    #[test]
    fn activity_preview_distinguishes_exploration_types() {
        let counts = ActivityCounts {
            explored_files: 2,
            explored_folders: 1,
            searches: 1,
            edited: 3,
            commands: 1,
            ..Default::default()
        };

        assert_eq!(
            activity_preview(&counts, false),
            "Explored 2 files, 1 folder, 1 search · Edited 3 files · Ran 1 command"
        );
        assert_eq!(
            activity_preview(&counts, true),
            "Reasoned · Explored 2 files, 1 folder, 1 search · Edited 3 files · Ran 1 command"
        );
    }
}
