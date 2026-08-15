//! ThemedButton component primitive for styled interactive buttons.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.PrimaryActionButton = Button {
        width: Fit
        height: 28
        padding: Inset{left: 12 right: 12 top: 4 bottom: 4}
        draw_bg +: {
            color: theme.color_primary
            color_hover: theme.color_primary
            color_focus: theme.color_primary
            color_down: theme.color_primary
            border_color: theme.color_primary
            border_radius: 6.0
        }
        draw_text +: {
            color: theme.color_primary_foreground
            color_hover: theme.color_primary_foreground
            color_focus: theme.color_primary_foreground
            color_down: theme.color_primary_foreground
            text_style: theme.font_bold { font_size: 9.5 }
        }
    }

    mod.components.RightSidebarTabButton = mod.components.IconButton {
        width: 36
        height: 28
        text: ""
        icon_walk: Walk{width: 12 height: 12}
        align: Align{x: 0.5 y: 0.5}
        padding: 0
        spacing: 0
        draw_icon +: {
            color: theme.color_muted_foreground
            color_hover: theme.color_foreground
            color_down: theme.color_primary
        }
    }

    mod.components.ToolbarIconButton = mod.components.IconButton {
        width: 26
        height: 26
        icon_walk: Walk{width: 14 height: 14}
    }
}
