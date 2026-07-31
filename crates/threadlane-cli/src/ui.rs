use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

pub struct AppState {
    pub model: String,
    pub work_dir: String,
    pub messages: Vec<TranscriptMessage>,
    pub input: String,
    pub is_generating: bool,
    pub scroll: u16,
}

#[derive(Clone, Debug)]
pub enum MessageType {
    User,
    Assistant,
    Thinking,
    ToolCall(String),
    Error,
}

#[derive(Clone, Debug)]
pub struct TranscriptMessage {
    pub msg_type: MessageType,
    pub content: String,
}

impl AppState {
    pub fn new(model: String, work_dir: String) -> Self {
        Self {
            model,
            work_dir,
            messages: vec![TranscriptMessage {
                msg_type: MessageType::Assistant,
                content: "Welcome to Threadlane CLI! Type your prompt below and press Enter to submit.".to_string(),
            }],
            input: String::new(),
            is_generating: false,
            scroll: 0,
        }
    }
}

pub fn render(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(5),    // Transcript
            Constraint::Length(3), // Prompt input
            Constraint::Length(1), // Footer shortcuts
        ])
        .split(frame.area());

    render_header(frame, state, chunks[0]);
    render_transcript(frame, state, chunks[1]);
    render_input(frame, state, chunks[2]);
    render_footer(frame, state, chunks[3]);
}

fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let status_str = if state.is_generating {
        Span::styled(" ● GENERATING ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" READY ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
    };

    let title = vec![
        Span::styled("Threadlane Agent", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" | Model: "),
        Span::styled(&state.model, Style::default().fg(Color::Magenta)),
        Span::raw(" | Workspace: "),
        Span::styled(&state.work_dir, Style::default().fg(Color::Gray)),
        Span::raw(" | "),
        status_str,
    ];

    let header_block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title))
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(header_block, area);
}

fn render_transcript(frame: &mut Frame, state: &AppState, area: Rect) {
    let mut lines = Vec::new();

    for msg in &state.messages {
        match &msg.msg_type {
            MessageType::User => {
                lines.push(Line::from(vec![
                    Span::styled("You: ", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                    Span::raw(&msg.content),
                ]));
            }
            MessageType::Assistant => {
                lines.push(Line::from(vec![
                    Span::styled("Threadlane: ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::raw(&msg.content),
                ]));
            }
            MessageType::Thinking => {
                lines.push(Line::from(vec![
                    Span::styled("Thinking: ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
                    Span::styled(&msg.content, Style::default().fg(Color::DarkGray)),
                ]));
            }
            MessageType::ToolCall(tool_name) => {
                lines.push(Line::from(vec![
                    Span::styled(format!("Tool ({tool_name}): "), Style::default().fg(Color::Yellow)),
                    Span::styled(&msg.content, Style::default().fg(Color::DarkGray)),
                ]));
            }
            MessageType::Error => {
                lines.push(Line::from(vec![
                    Span::styled("Error: ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                    Span::styled(&msg.content, Style::default().fg(Color::Red)),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Transcript "))
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));

    frame.render_widget(paragraph, area);
}

fn render_input(frame: &mut Frame, state: &AppState, area: Rect) {
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" Prompt ")
        .style(if state.is_generating {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Yellow)
        });

    let input_paragraph = Paragraph::new(state.input.as_str()).block(input_block);
    frame.render_widget(input_paragraph, area);
}

fn render_footer(frame: &mut Frame, _state: &AppState, area: Rect) {
    let footer_text = Line::from(vec![
        Span::styled("[Enter]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" Submit Prompt  "),
        Span::styled("[Esc / Ctrl+C]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" Quit  "),
        Span::styled("[Up/Down]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" Scroll Transcript"),
    ]);

    frame.render_widget(Paragraph::new(footer_text), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_initialization() {
        let state = AppState::new("gpt-4o".to_string(), "/tmp/work".to_string());
        assert_eq!(state.model, "gpt-4o");
        assert_eq!(state.work_dir, "/tmp/work");
        assert!(!state.is_generating);
        assert_eq!(state.messages.len(), 1);
    }

    #[test]
    fn test_message_types() {
        let msg = TranscriptMessage {
            msg_type: MessageType::Error,
            content: "Test Error".to_string(),
        };
        assert!(matches!(msg.msg_type, MessageType::Error));
    }
}
