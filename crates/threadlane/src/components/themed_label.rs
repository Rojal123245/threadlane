//! ThemedLabel component primitive for styled text labels.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ThemedLabel = Label {
        width: Fit
        height: Fit
        padding: 0
        align: Align{y: 0.5}
        draw_text +: {
            color: theme.color_foreground
            text_style +: { font_size: 9.5 }
        }
    }
}
