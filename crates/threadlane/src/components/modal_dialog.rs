//! Generic ModalDialog overlay and container primitives.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ModalDialogBackdrop = GaussRoundedView {
        width: Fill
        height: Fill
        draw_bg +: {
            blur_level: 5.2
            corner_radius: 0.0
            border_width: 0.0
            tint_color: theme.color_background
            tint_alpha: 0.16
            surface_alpha: 0.62
            fallback_color: theme.color_background
            shadow_radius: 0.0
            shadow_offset: vec2(0.0 0.0)
        }
    }

    mod.components.ModalDialogCard = RoundedView {
        width: 780
        height: 520
        flow: Right
        padding: 0
        spacing: 0
        draw_bg +: {
            color: theme.color_popover
            border_radius: theme.radius_xl
            border_size: 1.0
            border_color: theme.color_border
        }
    }

    mod.components.ModalDialogHeader = View {
        width: Fill
        height: Fit
        flow: Right
        align: Align{y: 0.5}

        title_lbl := Label {
            width: Fill
            height: Fit
            text: "Modal Title"
            draw_text +: {
                color: theme.color_foreground
                text_style: theme.font_bold { font_size: 14.0 }
            }
        }

        close_btn := Button {
            width: 26
            height: 26
            padding: 0
            spacing: 0
            text: ""
            align: Align{x: 0.5 y: 0.5}
            icon_walk: Walk{width: 12 height: 12}
            draw_bg +: {
                color: theme.color_transparent
                color_hover: theme.color_card
                color_down: theme.color_primary_tint
                border_color: theme.color_transparent
                border_size: 0.0
                border_radius: 4.0
            }
            draw_icon +: {
                svg: crate_resource("self:resources/icons/close.svg")
                color: theme.color_muted_foreground
                color_hover: theme.color_foreground
                color_down: theme.color_primary_foreground
            }
        }
    }
}
