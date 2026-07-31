use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum InputEvent {
    Submit,
    CancelOrQuit,
    Backspace,
    Character(char),
    ScrollUp,
    ScrollDown,
    Resize,
}

pub fn map_event(event: Event) -> Option<InputEvent> {
    match event {
        Event::Key(key) => map_key_event(key),
        Event::Resize(_, _) => Some(InputEvent::Resize),
        _ => None,
    }
}

pub fn map_key_event(key: KeyEvent) -> Option<InputEvent> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(InputEvent::CancelOrQuit);
    }
    match key.code {
        KeyCode::Enter => Some(InputEvent::Submit),
        KeyCode::Esc => Some(InputEvent::CancelOrQuit),
        KeyCode::Backspace => Some(InputEvent::Backspace),
        KeyCode::Char(character) => Some(InputEvent::Character(character)),
        KeyCode::Up => Some(InputEvent::ScrollUp),
        KeyCode::Down => Some(InputEvent::ScrollDown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cli_keys_to_input_events() {
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Some(InputEvent::Submit));
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), Some(InputEvent::CancelOrQuit));
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(InputEvent::CancelOrQuit));
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)), Some(InputEvent::Character('x')));
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)), Some(InputEvent::Backspace));
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)), Some(InputEvent::ScrollUp));
        assert_eq!(map_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), Some(InputEvent::ScrollDown));
    }

    #[test]
    fn maps_resize_and_ignores_unhandled_events() {
        assert_eq!(map_event(Event::Resize(80, 24)), Some(InputEvent::Resize));
        assert_eq!(map_event(Event::FocusGained), None);
    }
}
