//! AddServerCard component primitive for server/agent configuration cards.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.AddServerCard = RoundedView {
        width: Fill
        height: Fit
        flow: Down
        padding: Inset{left: 12 top: 10 right: 12 bottom: 10}
        spacing: 8
        draw_bg +: {
            color: theme.color_card
            border_radius: 8.0
            border_size: 1.0
            border_color: theme.color_input
        }

        add_title := Label {
            width: Fill
            height: Fit
            draw_text +: {
                color: theme.color_foreground
                text_style: theme.font_bold { font_size: 11.0 }
            }
        }

        add_inputs := View {
            width: Fill
            height: Fit
            flow: Right
            spacing: 8
            align: Align{y: 0.5}

            name_input := mod.components.ThemedTextInput {
                width: 140
            }

            command_input := mod.components.ThemedTextInput {
                width: Fill
            }

            submit_add_btn := mod.components.SecondaryActionButton {
                height: 28
                padding: Inset{left: 12 right: 12 top: 4 bottom: 4}
                text: "Add"
                draw_bg +: {
                    color: theme.color_primary
                    border_color: theme.color_primary
                }
                draw_text +: {
                    color: theme.color_primary_foreground
                    color_hover: theme.color_primary_foreground
                    color_focus: theme.color_primary_foreground
                    color_down: theme.color_primary_foreground
                }
            }
        }
    }
}
