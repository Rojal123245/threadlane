//! Compact context-window usage indicator with a hover details card.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.components.ContextWindowBase = #(ContextWindow::register_widget(vm))

    mod.components.ContextWindow = mod.components.ContextWindowBase {
        width: 28
        height: 28
        draw_ring +: {
            track_color: uniform(theme.color_input)
            progress_color: uniform(theme.color_primary)
            warning_color: uniform(theme.color_warning)
            danger_color: uniform(theme.color_destructive)
            progress: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let center = self.rect_size * 0.5
                let radius = min(self.rect_size.x, self.rect_size.y) * 0.5 - 3.0
                let progress = clamp(self.progress, 0.0, 1.0)
                let start_angle = -0.5 * PI
                let track_end = 1.5 * PI
                sdf.arc_round_caps(
                    center.x
                    center.y
                    radius
                    start_angle
                    track_end
                    3.0
                )
                sdf.fill(self.track_color)
                if progress > 0.0 {
                    let color = self.progress_color
                        .mix(self.warning_color, step(0.70, progress))
                        .mix(self.danger_color, step(0.90, progress))
                    let progress_end = start_angle + 2.0 * PI * progress
                    sdf.arc_round_caps(
                        center.x
                        center.y
                        radius
                        start_angle
                        progress_end
                        3.0
                    )
                    sdf.fill(color)
                }
                return sdf.result
            }
        }
        draw_tooltip_bg +: {
            color: uniform(theme.color_popover)
            border_color: uniform(theme.color_border)
            border_size: uniform(1.0)
            border_radius: uniform(9.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(
                    self.border_size,
                    self.border_size,
                    self.rect_size.x - self.border_size * 2.0,
                    self.rect_size.y - self.border_size * 2.0,
                    self.border_radius
                )
                sdf.fill_keep(self.color)
                sdf.stroke(self.border_color, self.border_size)
                return sdf.result
            }
        }
        draw_tooltip_track +: {
            color: uniform(theme.color_input)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 4.0)
                return sdf.fill(self.color)
            }
        }
        draw_tooltip_fill +: {
            color: uniform(theme.color_primary)
            warning_color: uniform(theme.color_warning)
            danger_color: uniform(theme.color_destructive)
            progress: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let progress = clamp(self.progress, 0.0, 1.0)
                let color = self.color
                    .mix(self.warning_color, step(0.70, progress))
                    .mix(self.danger_color, step(0.90, progress))
                sdf.box(0.0, 0.0, self.rect_size.x * progress, self.rect_size.y, 4.0)
                return sdf.fill(color)
            }
        }
        draw_title +: {
            color: theme.color_foreground
            text_style: theme.font_regular { font_size: 10.0 }
        }
        draw_usage +: {
            color: theme.color_foreground
            text_style: theme.font_regular { font_size: 9.5 }
        }
        draw_total +: {
            color: theme.color_muted_foreground
            text_style: theme.font_regular { font_size: 9.0 }
        }
        draw_hint +: {
            color: theme.color_muted_foreground
            text_style: theme.font_regular { font_size: 8.5 }
        }
    }
}

#[derive(Script, Widget)]
pub struct ContextWindow {
    #[uid]
    uid: WidgetUid,
    #[redraw]
    #[live]
    draw_ring: DrawQuad,
    #[live]
    draw_tooltip_bg: DrawQuad,
    #[live]
    draw_tooltip_track: DrawQuad,
    #[live]
    draw_tooltip_fill: DrawQuad,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_usage: DrawText,
    #[live]
    draw_total: DrawText,
    #[live]
    draw_hint: DrawText,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live(true)]
    visible: bool,
    #[rust]
    progress: f32,
    #[rust]
    usage_text: String,
    #[rust]
    total_text: String,
    #[rust]
    has_usage: bool,
    #[rust]
    hovered: bool,
    #[rust]
    hover_position: Vec2d,
    #[rust]
    draw_list: Option<DrawList2d>,
}

impl ScriptHook for ContextWindow {
    fn on_after_new(&mut self, vm: &mut ScriptVm) {
        self.draw_list = Some(DrawList2d::script_new(vm));
    }
}

impl ContextWindow {
    fn set_progress(&mut self, cx: &mut Cx, progress: f32) {
        self.progress = progress.clamp(0.0, 1.0);
        self.draw_ring
            .draw_vars
            .set_dyn_instance(cx, live_id!(progress), &[self.progress]);
        self.draw_tooltip_fill
            .draw_vars
            .set_dyn_instance(cx, live_id!(progress), &[self.progress]);
        self.draw_ring.redraw(cx);
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
    }
}

impl Widget for ContextWindow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_ring.area()) {
            Hit::FingerHoverIn(event) | Hit::FingerHoverOver(event) => {
                self.hovered = true;
                self.hover_position = event.abs;
                self.draw_ring.redraw(cx);
                if let Some(draw_list) = &self.draw_list {
                    draw_list.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.hovered = false;
                self.draw_ring.redraw(cx);
                if let Some(draw_list) = &self.draw_list {
                    draw_list.redraw(cx);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible {
            return DrawStep::done();
        }

        self.draw_ring.draw_walk(cx, walk);
        if !self.hovered || !self.has_usage {
            return DrawStep::done();
        }

        let Some(draw_list) = self.draw_list.as_mut() else {
            return DrawStep::done();
        };
        let pass_size = cx.current_pass_size();
        const TOOLTIP_WIDTH: f64 = 224.0;
        const TOOLTIP_HEIGHT: f64 = 138.0;
        const EDGE_GAP: f64 = 8.0;
        const TOOLTIP_GAP: f64 = 8.0;
        let x = (self.hover_position.x - TOOLTIP_WIDTH * 0.5).clamp(
            EDGE_GAP,
            (pass_size.x - TOOLTIP_WIDTH - EDGE_GAP).max(EDGE_GAP),
        );
        let above_y = self.hover_position.y - TOOLTIP_HEIGHT - TOOLTIP_GAP;
        let below_y = self.hover_position.y + TOOLTIP_GAP;
        let y = if above_y >= EDGE_GAP {
            above_y
        } else {
            below_y
        }
        .clamp(
            EDGE_GAP,
            (pass_size.y - TOOLTIP_HEIGHT - EDGE_GAP).max(EDGE_GAP),
        );
        let tooltip = Rect {
            pos: dvec2(x, y),
            size: dvec2(TOOLTIP_WIDTH, TOOLTIP_HEIGHT),
        };
        draw_list.begin_overlay_reuse(cx);
        cx.begin_root_turtle(pass_size, Layout::flow_down());
        self.draw_tooltip_bg.draw_abs(cx, tooltip);
        self.draw_title
            .draw_abs(cx, tooltip.pos + dvec2(16.0, 18.0), "Context Window");
        self.draw_usage
            .draw_abs(cx, tooltip.pos + dvec2(16.0, 43.0), &self.usage_text);
        self.draw_tooltip_track.draw_abs(
            cx,
            Rect {
                pos: tooltip.pos + dvec2(16.0, 64.0),
                size: dvec2(192.0, 8.0),
            },
        );
        self.draw_tooltip_fill.draw_abs(
            cx,
            Rect {
                pos: tooltip.pos + dvec2(16.0, 64.0),
                size: dvec2(192.0, 8.0),
            },
        );
        self.draw_total
            .draw_abs(cx, tooltip.pos + dvec2(16.0, 91.0), &self.total_text);
        self.draw_hint.draw_abs(
            cx,
            tooltip.pos + dvec2(16.0, 117.0),
            "Compacts automatically when needed.",
        );
        cx.end_pass_sized_turtle();
        draw_list.end(cx);
        DrawStep::done()
    }
}

impl ContextWindowRef {
    pub fn set_usage(&self, cx: &mut Cx, input_tokens: u32, total_tokens: u32, limit: u32) {
        if let Some(mut inner) = self.borrow_mut() {
            let limit = limit.max(1);
            inner.visible = true;
            inner.has_usage = true;
            inner.usage_text = format!(
                "{}% · {}/{}",
                (input_tokens as f32 / limit as f32 * 100.0).round() as u32,
                format_tokens(input_tokens),
                format_tokens(limit)
            );
            inner.total_text = format!("Total processed · {}", format_tokens(total_tokens));
            inner.set_progress(cx, input_tokens as f32 / limit as f32);
        }
    }

    pub fn clear_usage(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.visible = true;
            inner.has_usage = false;
            inner.hovered = false;
            inner.set_progress(cx, 0.0);
            inner.draw_ring.redraw(cx);
        }
    }
}

fn format_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f32 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", (tokens + 999) / 1_000)
    } else {
        tokens.to_string()
    }
}
