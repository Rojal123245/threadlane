//! SessionRowBase component for list row items.

use makepad_widgets::*;


script_mod! {
    use mod.prelude.widgets.*

    mod.components.SessionRowBase = RoundedView {
        width: Fill
        height: 34
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 10
        align: Align{y: 0.5}
        margin: Inset{left: 10 right: 4 top: 1 bottom: 1}
        padding: Inset{left: 20 top: 4 right: 9 bottom: 4}
        draw_bg +: {
            hover: instance(0.0)
            tree_last: instance(0.0)
            is_active: instance(0.0)
            color: theme.color_transparent
            color_hover: uniform(theme.color_card)
            tree_color: uniform(theme.color_card)
            border_radius: 7.0

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let tree_x = 9.0
                let surface_x = 14.0
                let surface_left = surface_x + self.border_size
                let surface_width = max(
                    0.0
                    self.rect_size.x - surface_x - self.border_size * 2.0
                )
                sdf.box(
                    surface_left
                    self.border_size
                    surface_width
                    self.rect_size.y - self.border_size * 2.0
                    max(1.0 self.border_radius)
                )
                sdf.fill_keep(mix(self.color, self.color_hover, self.hover))
                sdf.stroke(self.border_color, self.border_size)

                let tree_mid = self.rect_size.y * 0.5
                let tree_height = mix(self.rect_size.y, tree_mid, self.tree_last)
                sdf.rect(tree_x, 0.0, 1.0, max(0.0, tree_height))
                sdf.fill(self.tree_color)
                sdf.rect(tree_x, tree_mid, surface_x - tree_x + 1.0, 1.0)
                sdf.fill(self.tree_color)
                return sdf.result
            }
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward {duration: 0.10}}
                    apply: {draw_bg: {hover: 0.0}}
                }
                on: AnimatorState {
                    from: {all: Forward {duration: 0.08}}
                    apply: {draw_bg: {hover: snap(1.0)}}
                }
            }
        }
        title_surface := mod.components.SessionTitle {}
        session_row_spinner := mod.components.ActivityLoader {
            width: 18
            height: 10
            visible: false
        }
        health_lbl := Label {
            width: Fit
            height: Fit
            visible: false
            text: ""
            draw_text +: { color: theme.color_muted_foreground text_style +: { font_size: 8.0 } }
        }
        time_lbl := Label {
            width: Fit
            height: Fit
            text: ""
            draw_text +: { color: theme.color_muted_foreground text_style +: { font_size: 9.0 } }
        }
    }
}
