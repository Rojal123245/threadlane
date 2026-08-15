//! Overlay popup utility helpers for layout clamping and input dismissal.

use makepad_widgets::*;

/// Checks if a pointer event or keypress event should trigger dismissal of an overlay popup.
pub fn is_overlay_dismissal_event(event: &Event, popup_rect: Rect) -> bool {
    match event {
        Event::MouseUp(e) if e.button.is_primary() => !popup_rect.contains(e.abs),
        Event::KeyDown(e) if e.key_code == KeyCode::Escape => true,
        _ => false,
    }
}
