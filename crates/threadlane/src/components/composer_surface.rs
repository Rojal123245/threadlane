//! Minimal ComposerSurface card container component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ComposerSurface = RoundedView {
        width: Fill
        height: Fit
        draw_bg +: {
            color: theme.color_card
            border_color: theme.color_border
            border_size: 1.0
            border_radius: theme.radius_xl
        }
    }
}
