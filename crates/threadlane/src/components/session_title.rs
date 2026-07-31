//! Marquee-clipped SessionTitle component for session list rows.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SessionTitle = #(SessionTitle::register_widget(vm)) {
        width: Fill
        height: 18
        flow: Right
        align: Align{y: 0.5}
        clip_x: true
        clip_y: false
        padding: 0
        title_lbl := Label {
            width: Fit
            height: 16
            padding: 0
            text: ""
            draw_text +: { color: theme.color_card_foreground text_style +: { font_size: 10.5 } }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SessionTitle {
    #[deref]
    view: View,
    #[rust]
    hovered: bool,
    #[rust]
    offset: f64,
    #[rust]
    max_offset: f64,
    #[rust]
    phase_start: Option<f64>,
    #[rust]
    next_frame: NextFrame,
}

impl SessionTitle {
    const START_PAUSE: f64 = 0.45;
    const END_PAUSE: f64 = 0.65;
    const SPEED: f64 = 28.0;

    fn reset(&mut self, cx: &mut Cx) {
        self.offset = 0.0;
        self.phase_start = None;
        self.view.set_scroll_pos(cx, dvec2(0.0, 0.0));
        self.view.redraw(cx);
    }

    fn set_hovered(&mut self, cx: &mut Cx, hovered: bool) {
        if self.hovered == hovered {
            return;
        }

        self.hovered = hovered;
        self.reset(cx);
        if hovered && self.max_offset > 0.5 {
            self.next_frame = cx.new_next_frame();
        }
    }

    fn advance(&mut self, cx: &mut Cx, time: f64) {
        if !self.hovered || self.max_offset <= 0.5 {
            return;
        }

        let phase_start = *self.phase_start.get_or_insert(time);
        let travel_duration = self.max_offset / Self::SPEED;
        let elapsed = time - phase_start;
        let travel_start = Self::START_PAUSE;
        let travel_end = travel_start + travel_duration;
        let cycle_end = travel_end + Self::END_PAUSE;

        self.offset = if elapsed < travel_start {
            0.0
        } else if elapsed < travel_end {
            ((elapsed - travel_start) * Self::SPEED).min(self.max_offset)
        } else if elapsed < cycle_end {
            self.max_offset
        } else {
            self.phase_start = Some(time);
            0.0
        };

        self.view.set_scroll_pos(cx, dvec2(self.offset, 0.0));
        self.view.redraw(cx);
        self.next_frame = cx.new_next_frame();
    }
}

impl Widget for SessionTitle {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let title = self.view.label(cx, ids!(title_lbl));
        let text = title.text();
        let text_width = title
            .borrow()
            .and_then(|title| title.draw_text.prepare_single_line_run(cx, &text))
            .map(|run| run.width_in_lpxs as f64)
            .unwrap_or(0.0);

        self.view.set_scroll_pos(cx, dvec2(self.offset, 0.0));
        let step = self.view.draw_walk(cx, scope, walk);
        let viewport_width = self.view.area().rect(cx).size.x;
        self.max_offset = (text_width - viewport_width).max(0.0);
        self.offset = self.offset.min(self.max_offset);
        step
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if let Some(frame) = self.next_frame.is_event(event) {
            self.advance(cx, frame.time);
        }

        match event {
            Event::MouseMove(event) => {
                let hovered = self.view.area().rect(cx).contains(event.abs);
                self.set_hovered(cx, hovered);
            }
            Event::MouseLeave(_) => self.set_hovered(cx, false),
            _ => {}
        }
    }
}
