//! Minimal ComposerAction and ComposerChip button base components.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ComposerChip = Button {
        width: Fit
        height: 24
        padding: Inset{left: 9 right: 9 top: 2 bottom: 2}
        draw_bg +: {
            color: theme.color_secondary
            color_hover: theme.color_secondary
            color_down: theme.color_input
            border_color: theme.color_border
            border_color_hover: theme.color_input
            border_size: 1.0
            border_radius: 6.0
        }
        draw_text +: {
            color: theme.color_card_foreground
            color_hover: theme.color_foreground
            color_down: theme.color_primary_foreground
            text_style +: { font_size: 9.0 }
        }
    }

    mod.components.AttachmentChip = mod.components.ComposerChip {
        visible: false
        padding: Inset{left: 8 right: 9 top: 2 bottom: 2}
        icon_walk: Walk{width: 12 height: 12 margin: Inset{right: 5}}
        draw_icon +: {
            svg: crate_resource("self:resources/icons/image.svg")
            color: theme.color_primary
            color_hover: theme.color_primary
            color_down: theme.color_primary_foreground
        }
    }

    mod.components.ComposerAction = Button {
        width: Fit
        height: 28
        padding: Inset{left: 11 right: 11 top: 2 bottom: 2}
        draw_bg +: {
            color: theme.color_primary
            color_hover: theme.color_primary
            color_down: theme.color_primary
            border_radius: 7.0
        }
        draw_text +: {
            color: theme.color_primary_foreground
            text_style: theme.font_bold { font_size: 9.5 }
        }
    }
}
