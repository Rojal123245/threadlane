//! Shared starter-prompt card styling.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.StarterPromptCard = RoundedView {
        width: 160
        height: 108
        flow: Overlay
        draw_bg +: {
            color: theme.color_card
            border_color: theme.color_input
            border_size: 1.0
            border_radius: theme.radius_lg
        }
        content := View {
            width: Fill
            height: Fill
            flow: Down
            spacing: 10
            padding: Inset{left: 14 top: 14 right: 14 bottom: 14}
            header := View {
                width: Fill
                height: 26
                flow: Right
                spacing: 9
                align: Align{y: 0.5}
                icon_wrap := RoundedView {
                    width: 26
                    height: 26
                    align: Align{x: 0.5 y: 0.5}
                    draw_bg +: {
                        color: theme.color_primary_tint
                        border_color: theme.color_primary_tint
                        border_size: 1.0
                        border_radius: theme.radius_sm
                    }
                    icon := Icon {
                        width: 14
                        height: 14
                        icon_walk: Walk{width: 14 height: 14}
                        draw_icon +: { color: theme.color_primary }
                    }
                }
                title := Label {
                    width: Fill
                    height: Fit
                    draw_text +: {
                        color: theme.color_card_foreground
                        text_style: theme.font_bold { font_size: 11.0 }
                    }
                }
            }
            description := Label {
                width: Fill
                height: Fit
                draw_text +: {
                    color: theme.color_primary
                    text_style +: { font_size: 9.5 }
                }
            }
        }
        btn := Button {
            width: Fill
            height: Fill
            padding: 0
            spacing: 0
            icon_walk: Walk{width: 0 height: 0}
            draw_bg +: {
                color: theme.color_transparent
                border_color: theme.color_transparent
                border_size: 0.0
                border_radius: theme.radius_lg
            }
            draw_text +: { color: theme.color_transparent }
        }
    }
}
