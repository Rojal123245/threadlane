//! SettingsPage component primitive for settings/capabilities pages.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SettingsStatusLabel = Label {
        width: Fill
        height: Fit
        padding: 0
        text: ""
        draw_text +: {
            color: theme.color_primary
            text_style +: { font_size: 9.5 }
        }
    }
}
