//! ToolSection container component for payload details (INPUT / OUTPUT).

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ToolSection = RoundedView {
        width: Fill
        height: Fit
        flow: Down
        spacing: 4
        padding: Inset{left: 8 top: 5 right: 8 bottom: 6}
        draw_bg +: {
            color: theme.color_background
            border_color: theme.color_card
            border_radius: 4.0
            border_size: 1.0
        }
        section_label := Label {
            width: Fill
            height: Fit
            text: "SECTION"
            draw_text +: {
                color: theme.color_primary
                text_style: theme.font_bold { font_size: 7.5 }
            }
        }
        content_lbl := mod.components.CodeLabel {
            width: Fill
        }
        content_md_wrap := View {
            width: Fill
            height: Fit
            visible: false
            content_md := mod.components.ChatMarkdown {}
        }
    }
}
