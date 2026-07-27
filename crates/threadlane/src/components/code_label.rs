//! CodeLabel component primitive for monospace text rendering.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.CodeLabel = Label {
        width: Fit
        height: Fit
        text: ""
        draw_text +: {
            color: theme.color_muted_foreground
            text_style: theme.font_code { font_size: 8.5 }
        }
    }
}
