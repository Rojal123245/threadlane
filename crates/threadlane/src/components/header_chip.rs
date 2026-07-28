//! Shared header action chip button.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.HeaderChipButton = Button {
        width: Fit
        height: 28
        padding: Inset{left: 9 right: 9 top: 4 bottom: 4}
        spacing: 5
        icon_walk: Walk{width: 13 height: 13}
        draw_icon +: {
            color: theme.color_primary
            color_hover: theme.color_primary
            color_focus: theme.color_primary
            color_down: theme.color_primary_foreground
        }
        draw_bg +: {
            color: theme.color_secondary
            color_hover: theme.color_card
            color_focus: theme.color_card
            color_down: theme.color_primary
            border_color: theme.color_secondary
            border_color_hover: theme.color_primary
            border_color_focus: theme.color_primary
            border_color_down: theme.color_primary
            border_radius: 7.0
            border_size: 1.0
        }
        draw_text +: {
            color: theme.color_foreground
            color_hover: theme.color_primary_foreground
            color_focus: theme.color_primary_foreground
            color_down: theme.color_primary_foreground
            text_style: theme.font_bold { font_size: 9.5 }
        }
    }
}
