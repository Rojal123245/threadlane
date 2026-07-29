//! Generic SearchInput filter text input component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SearchInput = TextInput {
        width: Fill
        height: 26
        empty_text: "Filter…"
        padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
        margin: Inset{left: 8 right: 8 top: 2 bottom: 6}
        draw_bg +: {
            color: theme.color_background
            color_hover: theme.color_background
            color_focus: theme.color_background
            border_color: theme.color_border
            border_color_focus: theme.color_primary
            border_radius: 4.0
            border_size: 1.0
        }
        draw_text +: {
            color: theme.color_foreground
            color_empty: theme.color_muted_foreground
            text_style +: {
                font_size: 8.5
            }
        }
    }
}
