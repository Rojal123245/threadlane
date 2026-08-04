//! ActivityLoader and ProgressDot status indicator components.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ProgressDot = RoundedView {
        width: 3
        height: 3
        draw_bg +: { color: theme.color_muted_foreground border_radius: 1.5 }
    }

    mod.components.ActivityLoader = #(ActivityLoader::register_widget(vm)) {
        width: 20
        height: 10
        show_bg: true
        draw_bg +: {
            color: uniform(theme.color_primary)
            color_mid: uniform(theme.color_foreground)
            color_tail: uniform(theme.color_muted_foreground)
            color_idle: uniform(theme.color_muted)
            speed: uniform(7.0)
            dot_radius: uniform(1.15)

            pixel: fn() {
                let p = self.pos * self.rect_size
                let padding = max(1.0, min(self.rect_size.x, self.rect_size.y) * 0.10)
                let content_size = max(
                    vec2(1.0, 1.0),
                    self.rect_size - vec2(padding * 2.0, padding * 2.0)
                )
                let grid_size = vec2(6.0, 4.0)
                let cell_size = content_size / grid_size
                let grid = (p - vec2(padding, padding)) / cell_size

                if grid.x < 0.0 || grid.y < 0.0 || grid.x >= 6.0 || grid.y >= 4.0 {
                    return theme.color_transparent
                }

                let column = floor(grid.x)
                let row = floor(grid.y)
                let odd_row = row - floor(row * 0.5) * 2.0
                let snake_column = if odd_row < 0.5 column else 5.0 - column
                let index = row * 6.0 + snake_column
                let center = vec2(padding, padding)
                    + (vec2(column, row) + vec2(0.5, 0.5)) * cell_size
                let radius = min(self.dot_radius, min(cell_size.x, cell_size.y) * 0.34)
                let distance = length(p - center)
                let coverage = smoothstep(radius + 0.65, radius - 0.35, distance)

                let phase = fract(self.draw_pass.time * self.speed / 24.0) * 24.0
                let age = if phase >= index phase - index else phase + 24.0 - index
                let head = smoothstep(1.0, 0.0, age)
                let trail = smoothstep(5.0, 0.0, age) * (1.0 - head)
                let active_color = self.color_mid.mix(self.color, head)
                let dot_color = self.color_idle
                    .mix(self.color_tail, trail)
                    .mix(active_color, max(head, trail * 0.72))
                let alpha = coverage

                return Pal.premul(vec4(dot_color.xyz, alpha))
            }
        }
    }

    mod.components.ActivityStatusIndicator = View {
        width: Fit
        height: Fit
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        spacing: 4

        status_running_indicator := mod.components.ActivityLoader {
            width: 14
            height: 10
            visible: false
            draw_bg +: {
                dot_radius: 1.0
                speed: 3.6
            }
        }
        status_done_indicator := mod.components.StatusDot {
            width: 5
            height: 5
            draw_bg +: {
                color: theme.color_success
                border_radius: 2.5
            }
        }
        status_cancelled_indicator := mod.components.StatusDot {
            width: 5
            height: 5
            visible: false
            draw_bg +: {
                color: theme.color_muted_foreground
                border_radius: 2.5
            }
        }
        status_error_lbl := Label {
            width: Fit
            height: Fit
            visible: false
            text: "!"
            draw_text +: {
                color: theme.color_destructive
                text_style: theme.font_bold { font_size: 8.0 }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ActivityLoader {
    #[deref]
    view: View,
    #[rust]
    next_frame: NextFrame,
}

impl Widget for ActivityLoader {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.view.visible() {
            self.next_frame = cx.new_next_frame();
        }
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if self.next_frame.is_event(event).is_some() && self.view.visible() {
            self.view.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }
}
