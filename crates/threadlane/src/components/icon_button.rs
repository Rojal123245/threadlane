//! Base reusable IconButton primitive for compact icon-only buttons.
//!
//! Enforces centered SVG viewbox alignment and standard hit-test padding
//! according to repository Makepad component conventions.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.IconButton = Button {
        width: 26
        height: 26
        margin: 0
        padding: 0
        spacing: 0
        text: ""
        align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 14 height: 14 margin: 0}
        draw_icon +: {
            color: theme.color_muted_foreground
            color_hover: theme.color_foreground
            color_focus: theme.color_foreground
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
        animator: Animator{
            selected: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.1}}
                    apply: {
                        draw_bg: {
                            color: theme.color_transparent
                            color_hover: theme.color_secondary
                            color_focus: theme.color_secondary
                            color_down: theme.color_input
                        }
                        draw_icon: {
                            color: theme.color_muted_foreground
                            color_hover: theme.color_foreground
                            color_focus: theme.color_foreground
                            color_down: theme.color_primary_foreground
                        }
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.1}}
                    apply: {
                        draw_bg: {
                            color: theme.color_secondary
                            color_hover: theme.color_card
                            color_focus: theme.color_card
                            color_down: theme.color_input
                        }
                        draw_icon: {
                            color: theme.color_foreground
                            color_hover: theme.color_foreground
                            color_focus: theme.color_foreground
                            color_down: theme.color_primary_foreground
                        }
                    }
                }
            }
        }
    }
}
