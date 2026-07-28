//! Shared navigation button styling and selected-state synchronization.

use makepad_widgets::*;

/// Apply the persistent selected state without replacing the underlying Button.
pub fn set_selected(cx: &mut Cx, button: &ButtonRef, selected: bool) {
    if let Some(mut button) = button.borrow_mut() {
        button.animator_play(
            cx,
            if selected {
                ids!(selected.on)
            } else {
                ids!(selected.off)
            },
        );
    }
}

script_mod! {
    use mod.prelude.widgets.*

    mod.components.NavButton = Button {
        width: Fill
        height: 34
        padding: Inset{left: 10 right: 8 top: 6 bottom: 6}
        spacing: 0
        align: Align{x: 0.0 y: 0.5}

        draw_bg +: {
            border_size: 1.0
            border_radius: 6.0
            color: theme.color_background
            color_hover: theme.color_card
            color_focus: theme.color_secondary
            color_down: theme.color_primary
            border_color: theme.color_border
            border_color_hover: theme.color_primary
            border_color_focus: theme.color_primary
            border_color_down: theme.color_primary
        }
        draw_text +: {
            color: theme.color_primary
            color_hover: theme.color_foreground
            color_focus: theme.color_primary_foreground
            color_down: theme.color_primary_foreground
            text_style +: { font_size: 9.5 }
        }

        animator: Animator{
            selected: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.}}
                    apply: {
                        draw_bg: {
                            color: theme.color_background
                            color_hover: theme.color_card
                            color_focus: theme.color_secondary
                            color_down: theme.color_primary
                            border_color: theme.color_border
                            border_color_hover: theme.color_primary
                            border_color_focus: theme.color_primary
                            border_color_down: theme.color_primary
                        }
                        draw_text: {
                            color: theme.color_primary
                            color_hover: theme.color_foreground
                            color_focus: theme.color_primary_foreground
                            color_down: theme.color_primary_foreground
                        }
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.}}
                    apply: {
                        draw_bg: {
                            color: theme.color_primary
                            color_hover: theme.color_primary
                            color_focus: theme.color_primary
                            color_down: theme.color_primary
                            border_color: theme.color_primary
                            border_color_hover: theme.color_primary
                            border_color_focus: theme.color_primary
                            border_color_down: theme.color_primary
                        }
                        draw_text: {
                            color: theme.color_primary_foreground
                            color_hover: theme.color_primary_foreground
                            color_focus: theme.color_primary_foreground
                            color_down: theme.color_primary_foreground
                        }
                    }
                }
            }
        }
    }
}
