//! ThemedButton component primitive for styled interactive buttons.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ThemedButton = Button {
        width: Fit
        height: 28
        padding: Inset{left: 8 right: 8 top: 4 bottom: 4}
        align: Align{x: 0.5 y: 0.5}
        draw_bg +: {
            color: theme.color_card
            color_hover: theme.color_secondary
            color_focus: theme.color_secondary
            color_down: theme.color_input
            border_color: theme.color_border
            border_color_hover: theme.color_primary
            border_color_focus: theme.color_primary
            border_color_down: theme.color_primary
            border_size: 0.0
            border_radius: 5.0
        }
        draw_text +: {
            color: theme.color_foreground
            color_hover: theme.color_foreground
            color_focus: theme.color_primary_foreground
            color_down: theme.color_primary_foreground
            text_style +: { font_size: 9.0 }
        }
    }
}
