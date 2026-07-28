//! Base reusable IconButton primitive for compact icon-only buttons.
//!
//! Enforces centered SVG viewbox alignment and standard hit-test padding
//! according to repository Makepad component conventions.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.IconButton = Button {
        width: 24
        height: 24
        margin: 0
        padding: 0
        spacing: 0
        text: ""
        align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 12 height: 12 margin: 0}
        draw_icon +: {
            color: theme.color_primary
            color_hover: theme.color_primary
            color_down: theme.color_primary_foreground
        }
        draw_bg +: {
            color: theme.color_transparent
            color_hover: theme.color_secondary
            color_focus: theme.color_secondary
            color_down: theme.color_input
            border_color: theme.color_transparent
            border_color_hover: theme.color_transparent
            border_color_focus: theme.color_transparent
            border_color_down: theme.color_transparent
            border_size: 0.0
            border_radius: 6.0
        }
    }
}
