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
            spacing: 6
            align: Align{y: 0.5}
        }
    }
}
