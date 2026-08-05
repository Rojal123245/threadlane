//! ThemedTextInput component primitive for styled text inputs.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ThemedTextInput = TextInput {
        height: 28
        padding: Inset{left: 8 right: 8}
        draw_bg +: {
            color: theme.color_input
            color_focus: theme.color_input
            border_color: theme.color_secondary
            border_color_focus: theme.color_primary
            border_radius: 5.0
            border_size: 1.0
        }
        draw_text +: {
            color: theme.color_foreground
            color_empty: theme.color_muted_foreground
        }
    }
}
