use super::state::{ActivityStatus, AppState, MessageType, RunStatus};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use threadlane_agent::PlanItemStatus;

pub fn render(frame: &mut Frame, state: &AppState) {
    let sections = layout_sections(frame.area(), state);
    render_header(frame, state, sections.header);
    render_transcript(frame, state, sections.transcript);
    render_activity(frame, state, sections.activity);
    render_plan(frame, state, sections.plan);
    render_input(frame, state, sections.composer);
    render_footer(frame, sections.footer);
}

#[derive(Debug, PartialEq, Eq)]
pub struct LayoutSections {
    pub header: Rect,
    pub transcript: Rect,
    pub activity: Rect,
    pub plan: Rect,
    pub composer: Rect,
    pub footer: Rect,
}

pub fn layout_sections(area: Rect, state: &AppState) -> LayoutSections {
    let has_activity = !state.activities.is_empty();
    let has_plan = state
        .plan
        .as_ref()
        .is_some_and(|plan| !plan.items.is_empty());
    let mut constraints = vec![Constraint::Length(3), Constraint::Min(1)];
    if has_activity {
        constraints.push(Constraint::Length(section_height(
            state.activities.len(),
            5,
        )));
    }
    if has_plan {
        constraints.push(Constraint::Length(section_height(
            state.plan.as_ref().unwrap().items.len(),
            5,
        )));
    }
    constraints.extend([Constraint::Length(3), Constraint::Length(1)]);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(area);
    let header = chunks[0];
    let transcript = chunks[1];
    let mut next = 2;
    let empty = |y| Rect::new(transcript.x, y, transcript.width, 0);
    let activity = if has_activity {
        let rect = chunks[next];
        next += 1;
        rect
    } else {
        empty(transcript.y + transcript.height)
    };
    let plan = if has_plan {
        let rect = chunks[next];
        next += 1;
        rect
    } else {
        empty(activity.y + activity.height)
    };
    LayoutSections {
        header,
        transcript,
        activity,
        plan,
        composer: chunks[next],
        footer: chunks[next + 1],
    }
}

fn section_height(items: usize, cap: u16) -> u16 {
    items.saturating_add(2).min(cap as usize) as u16
}

fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let status = match state.status {
        RunStatus::Running => (" ● GENERATING ", Color::Yellow),
        RunStatus::Failed => (" FAILED ", Color::Red),
        RunStatus::Cancelled => (" CANCELLED ", Color::Yellow),
        RunStatus::Succeeded => (" DONE ", Color::Green),
        RunStatus::Idle | RunStatus::Ready => (" READY ", Color::Green),
    };
    let title = Line::from(vec![
        Span::styled(
            "Threadlane Agent",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | Model: "),
        Span::styled(&state.model, Style::default().fg(Color::Magenta)),
        Span::raw(" | Workspace: "),
        Span::styled(&state.work_dir, Style::default().fg(Color::Gray)),
        Span::raw(" | "),
        Span::styled(
            status.0,
            Style::default().fg(status.1).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(Color::Cyan)),
        area,
    );
}

fn render_transcript(frame: &mut Frame, state: &AppState, area: Rect) {
    let mut lines = Vec::new();
    for msg in &state.messages {
        let (label, color, modifier) = match &msg.msg_type {
            MessageType::User => ("You: ", Color::Blue, Modifier::BOLD),
            MessageType::Assistant => ("Threadlane: ", Color::Green, Modifier::BOLD),
            MessageType::Thinking => ("Thinking: ", Color::DarkGray, Modifier::ITALIC),
            MessageType::ToolCall(_) => ("Tool: ", Color::Yellow, Modifier::empty()),
            MessageType::Error => ("Error: ", Color::Red, Modifier::BOLD),
        };
        let detail = match &msg.msg_type {
            MessageType::ToolCall(name) => format!("{name}: {}", msg.content),
            _ => msg.content.clone(),
        };
        lines.push(Line::from(vec![
            Span::styled(label, Style::default().fg(color).add_modifier(modifier)),
            Span::raw(detail),
        ]));
        lines.push(Line::raw(""));
    }
    if let Some(streaming) = &state.streaming {
        if !streaming.reasoning.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("Thinking: {}", streaming.reasoning),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if !streaming.text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "Threadlane: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&streaming.text),
            ]));
        }
    }
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let total_lines = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(inner_width))
        .sum::<usize>();
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(viewport);
    let scroll = if state.follow_tail {
        max_scroll
    } else {
        max_scroll.saturating_sub(state.scroll as usize)
    } as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Transcript "))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn render_activity(frame: &mut Frame, state: &AppState, area: Rect) {
    if state.activities.is_empty() || area.height == 0 {
        return;
    }
    let lines = state
        .activities
        .iter()
        .map(|item| {
            let (status, color) = match item.status {
                ActivityStatus::Queued => ("queued", Color::DarkGray),
                ActivityStatus::Running => ("running", Color::Yellow),
                ActivityStatus::Succeeded => ("done", Color::Green),
                ActivityStatus::Failed => ("failed", Color::Red),
                ActivityStatus::Cancelled => ("cancelled", Color::Yellow),
            };
            Line::from(vec![
                Span::styled(format!("{status:9}"), Style::default().fg(color)),
                Span::styled(
                    format!("{}: ", item.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(&item.detail),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Activity "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_plan(frame: &mut Frame, state: &AppState, area: Rect) {
    let Some(plan) = &state.plan else {
        return;
    };
    if plan.items.is_empty() || area.height == 0 {
        return;
    }
    let lines = plan
        .items
        .iter()
        .map(|item| {
            let (marker, color) = match item.status {
                PlanItemStatus::Pending => ("[ ]", Color::DarkGray),
                PlanItemStatus::InProgress => ("[>]", Color::Yellow),
                PlanItemStatus::Completed => ("[x]", Color::Green),
            };
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(&item.step),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Plan "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_input(frame: &mut Frame, state: &AppState, area: Rect) {
    let color = if matches!(state.status, RunStatus::Running) {
        Color::DarkGray
    } else {
        Color::Yellow
    };
    frame.render_widget(
        Paragraph::new(state.composer.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Prompt ")
                .style(Style::default().fg(color)),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "[Enter]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Submit Prompt  "),
            Span::styled(
                "[Esc / Ctrl+C]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Quit  "),
            Span::styled(
                "[Up/Down]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Scroll Transcript"),
        ])),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_activity_and_plan_do_not_create_empty_sections() {
        let state = AppState::test_state();
        let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
        assert_eq!(sections.activity.height, 0);
        assert_eq!(sections.plan.height, 0);
        assert!(sections.transcript.height > 0);
        assert_eq!(sections.composer.height, 3);

        let mut empty_plan = AppState::test_state();
        empty_plan.plan = Some(threadlane_agent::SessionPlan {
            explanation: None,
            items: vec![],
        });
        let empty_plan_sections = layout_sections(Rect::new(0, 0, 100, 30), &empty_plan);
        assert_eq!(empty_plan_sections.activity.height, 0);
        assert_eq!(empty_plan_sections.plan.height, 0);
        assert_eq!(empty_plan_sections.transcript, sections.transcript);
        assert_eq!(empty_plan_sections.composer, sections.composer);
    }

    #[test]
    fn active_plan_and_activity_get_bounded_height() {
        let mut state = AppState::test_state_with_plan(20);
        state.activities = (0..20)
            .map(|index| super::super::state::ActivityItem {
                id: index.to_string(),
                name: "tool".into(),
                detail: "detail".into(),
                status: super::super::state::ActivityStatus::Running,
            })
            .collect();
        let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
        assert!(sections.transcript.height >= 1);
        assert!(sections.plan.height < 30);
        assert!(sections.activity.height < 30);
    }

    #[test]
    fn follow_tail_tracks_manual_scroll_back_to_end() {
        let mut state = AppState::test_state();
        assert!(state.follow_tail);
        state.scroll_up();
        assert!(!state.follow_tail);
        assert_eq!(state.scroll, 1);
        state.scroll_down();
        assert!(state.follow_tail);
        assert_eq!(state.scroll, 0);
    }
}
