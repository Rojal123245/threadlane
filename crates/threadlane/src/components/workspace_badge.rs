//! WorkspaceBadge component for workspace path chips.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.WorkspaceBadge = RoundedView {
        width: Fit
        height: Fit
        flow: Right
        align: Align{y: 0.5}
        spacing: 6
        margin: Inset{bottom: 24}
        padding: Inset{left: 10 top: 4 right: 10 bottom: 4}
        draw_bg +: {
            color: theme.color_card
            border_color: theme.color_border
            border_size: 1.0
            border_radius: theme.radius_sm
        }
        workspace_folder_icon := Icon {
            width: 14
            height: 14
            icon_walk: Walk{width: 14 height: 14}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/folder.svg")
                color: theme.color_primary
            }
        }
        workspace_path_lbl := Label {
            width: Fit
            height: Fit
            text: ""
            draw_text +: {
                color: theme.color_muted_foreground
                text_style +: { font_size: 10.0 }
            }
        }
    }
}
