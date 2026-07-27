//! Shared rows for project capabilities and skills.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    mod.components.CapabilityRowBase = RoundedView {
        width: Fill
        height: 72
        flow: Right
        spacing: 10
        padding: Inset{left: 12 top: 9 right: 8 bottom: 9}
        align: Align{y: 0.5}
        draw_bg +: {
            color: theme.color_background
            border_color: theme.color_card
            border_size: 1.0
            border_radius: 7.0
        }

        capability_text := View {
            width: Fill
            height: Fill
            flow: Down
            spacing: 3

            name_lbl := mod.components.ClippedLabel {
                height: 17
                padding: 0
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_foreground
                    text_style: theme.font_bold { font_size: 10.5 }
                }
            }
            scope_lbl := mod.components.ClippedLabel {
                height: 15
                padding: 0
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_primary
                    text_style +: { font_size: 9.0 }
                }
            }
            path_lbl := mod.components.ClippedLabel {
                height: 15
                padding: 0
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_muted_foreground
                    text_style: theme.font_code { font_size: 8.0 }
                }
            }
        }

        enabled_toggle := Toggle {
            width: 34
            height: 24
            padding: 0
            text: ""
            label_walk: Walk{width: 0 height: 0}
            draw_bg +: {
                color_active: theme.color_primary
                border_color_active: theme.color_primary
                mark_color_active: theme.color_primary_foreground
                mark_color_active_hover: theme.color_primary_foreground
            }
            animator +: {
                hover: {
                    on: AnimatorState {
                        from: {all: Snap}
                        apply: {
                            draw_bg: {down: snap(0.0), hover: 0.0}
                            draw_text: {down: snap(0.0), hover: 1.0}
                        }
                    }
                }
            }
        }
    }

    mod.components.CapabilityRowWithRemove = mod.components.CapabilityRowBase {
        remove_btn := mod.components.IconButton {
            draw_icon +: {
                svg: crate_resource("self:resources/icons/trash.svg")
                color_hover: theme.color_destructive
                color_focus: theme.color_destructive
                color_down: theme.color_primary_foreground
            }
        }
    }

    mod.components.CapabilityEmptyRow = View {
        width: Fill
        height: 72
        align: Align{x: 0.5 y: 0.5}
        empty_lbl := Label {
            width: Fit
            height: Fit
            draw_text +: {
                color: theme.color_muted_foreground
                text_style +: { font_size: 10.0 }
            }
        }
    }
}
