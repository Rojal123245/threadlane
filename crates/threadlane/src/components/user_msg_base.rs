//! UserMsgBase component primitive for user chat message bubbles.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.UserMsgBase = View {
        width: Fill
        height: Fit
        align: Align{x: 1.0}
        margin: Inset{top: 5 bottom: 5 left: 28 right: 20}

        user_bubble := ChatBubble {
            width: Fit{max: FitBound.Abs(680)}
            padding: Inset{left: 13 top: 8 right: 13 bottom: 8}
            md +: {
                width: Fit{max: FitBound.Abs(654)}
            }
            draw_bg +: {
                color: theme.color_card
                border_radius: 9.0
            }
        }
    }
}
