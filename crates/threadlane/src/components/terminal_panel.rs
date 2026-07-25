//! Compact, expandable project terminal surface with project-scoped tabs.

use makepad_terminal_core::{TermKeyCode as TerminalKeyCode, Terminal};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    mod.components.ProjectTerminalBase = #(ProjectTerminal::register_widget(vm))

    mod.components.TerminalTabButton = mod.widgets.Button {
        width: Fit height: 30 padding: Inset{left: 11 right: 8} spacing: 0
        draw_text +: { color: #xaeb8c5 color_hover: #edf2f8 color_focus: #edf2f8 color_down: #ffffff text_style: theme.font_code {font_size: 9.0} }
        draw_bg +: {
            color: #x00000000 color_hover: #x292f38 color_focus: #x2d3540 color_down: #x36404d
            border_color: #x00000000 border_color_hover: #x00000000 border_color_focus: #x00000000 border_color_down: #x00000000
            border_size: 0.0 border_radius: 4.0
        }
    }

    mod.components.TerminalTabSlot = RoundedView {
        width: Fit height: 34 flow: Right spacing: 0 padding: 0 align: Align{y: 0.5}
        draw_bg +: {
            color: #x20242a border_color: #x303741 border_size: 1.0 border_radius: 5.0
        }
    }

    mod.components.TerminalTabCloseButton = mod.widgets.Button {
        width: 22 height: 30 padding: 0 spacing: 0 text: "" align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 9 height: 9}
        draw_icon +: { color: #x8390a0 color_hover: #xe3e9f0 color_focus: #xe3e9f0 color_down: #xffffff }
        draw_bg +: {
            color: #x00000000 color_hover: #x313a46 color_focus: #x313a46 color_down: #x3c4857
            border_color: #x00000000 border_color_hover: #x00000000 border_color_focus: #x00000000 border_color_down: #x00000000
            border_size: 0.0 border_radius: 4.0
        }
    }

    mod.components.TerminalIconButton = mod.widgets.Button {
        width: 28 height: 28 padding: 0 spacing: 0 text: "" align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 12 height: 12}
        draw_icon +: { color: #x8c98a8 color_hover: #xdde3ea color_focus: #xdde3ea color_down: #xffffff }
        draw_bg +: {
            color: #x00000000 color_hover: #x252b33 color_focus: #x2b333e color_down: #x35414e
            border_color: #x00000000 border_color_hover: #x3a4655 border_color_focus: #x50657d border_color_down: #x6e8ba8 border_radius: 5.0
        }
    }

    mod.components.ProjectTerminal = set_type_default() do mod.components.ProjectTerminalBase {
        width: Fill height: Fit flow: Down spacing: 4

        terminal_toggle := mod.components.TerminalIconButton {
            width: 1
            height: 1
            icon_walk: Walk{width: 1 height: 1}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/terminal.svg")
                color: #x00000000
            }
        }

        terminal_resize_handle := View {
            width: Fill height: 6 visible: false
            show_bg: true
            draw_bg +: {
                color: #x2a3440
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.clear(#x00000000)
                    sdf.rect(0.0, self.rect_size.y * 0.5 - 0.5, self.rect_size.x, 1.0)
                    return sdf.fill(self.color)
                }
            }
        }

        terminal_body := RoundedView {
            width: Fill height: 250 visible: false flow: Down
            draw_bg +: { color: #x15181c border_color: #x303945 border_size: 1.0 border_radius: 9.0 }

            terminal_tabs := View {
                width: Fill height: 40 flow: Right padding: Inset{left: 8 top: 3 right: 8 bottom: 3} spacing: 5 align: Align{y: 0.5}
                tab_slot_0 := mod.components.TerminalTabSlot {visible: false tab_0 := mod.components.TerminalTabButton{text: ""} close_0 := mod.components.TerminalTabCloseButton{draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_1 := mod.components.TerminalTabSlot {visible: false tab_1 := mod.components.TerminalTabButton{text: ""} close_1 := mod.components.TerminalTabCloseButton{draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_2 := mod.components.TerminalTabSlot {visible: false tab_2 := mod.components.TerminalTabButton{text: ""} close_2 := mod.components.TerminalTabCloseButton{draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_3 := mod.components.TerminalTabSlot {visible: false tab_3 := mod.components.TerminalTabButton{text: ""} close_3 := mod.components.TerminalTabCloseButton{draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_4 := mod.components.TerminalTabSlot {visible: false tab_4 := mod.components.TerminalTabButton{text: ""} close_4 := mod.components.TerminalTabCloseButton{draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_5 := mod.components.TerminalTabSlot {visible: false tab_5 := mod.components.TerminalTabButton{text: ""} close_5 := mod.components.TerminalTabCloseButton{draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                terminal_new := mod.components.TerminalIconButton {
                    width: 22 height: 22
                    icon_walk: Walk{width: 9 height: 9}
                    draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}
                }
            }

            terminal_rule := View {width: Fill height: 1 show_bg: true draw_bg +: {color: #x282f38}}
            terminal_content := View {
                width: Fill height: Fill flow: Down padding: Inset{left: 13 top: 10 right: 13 bottom: 10} spacing: 6
                terminal_scroll := ScrollYView {
                    width: Fill height: Fill
                    terminal_output := Label {
                        width: Fill height: Fit text: ""
                        draw_text +: {color: #xd2d7dd text_style: theme.font_code {font_size: 9.5 line_spacing: 1.3}}
                    }
                }
            }
        }
    }
}

const MAX_VISIBLE_TERMINALS: usize = 6;

fn tab_id(index: usize) -> &'static [LiveId] {
    match index {
        0 => ids!(tab_0),
        1 => ids!(tab_1),
        2 => ids!(tab_2),
        3 => ids!(tab_3),
        4 => ids!(tab_4),
        _ => ids!(tab_5),
    }
}

fn close_id(index: usize) -> &'static [LiveId] {
    match index {
        0 => ids!(close_0),
        1 => ids!(close_1),
        2 => ids!(close_2),
        3 => ids!(close_3),
        4 => ids!(close_4),
        _ => ids!(close_5),
    }
}

fn slot_id(index: usize) -> &'static [LiveId] {
    match index {
        0 => ids!(tab_slot_0),
        1 => ids!(tab_slot_1),
        2 => ids!(tab_slot_2),
        3 => ids!(tab_slot_3),
        4 => ids!(tab_slot_4),
        _ => ids!(tab_slot_5),
    }
}

#[derive(Clone, Debug, Default)]
pub enum ProjectTerminalAction {
    Input(Vec<u8>),
    LayoutChanged,
    New,
    Select(usize),
    Close(usize),
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ProjectTerminal {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    expanded: bool,
    #[rust]
    focus_next_frame: NextFrame,
    #[rust]
    cursor_next_frame: NextFrame,
    #[rust]
    cursor_last_blink: f64,
    #[rust]
    cursor_blink_on: bool,
    #[rust]
    terminal_focused: bool,
    #[rust]
    output: String,
    #[rust]
    terminal_height: f64,
    #[rust]
    resizing: bool,
    #[rust]
    resize_start_y: f64,
    #[rust]
    resize_start_height: f64,
}

impl Widget for ProjectTerminal {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.focus_next_frame.is_event(event).is_some() && self.expanded {
            self.view.button(cx, ids!(terminal_toggle)).set_key_focus(cx);
        }
        let terminal_has_focus = self.view.button(cx, ids!(terminal_toggle)).key_focus(cx);
        if self.expanded && terminal_has_focus {
            match event {
                Event::KeyDown(key) => {
                    if let Some(bytes) = encode_key(key) {
                        cx.widget_action(self.widget_uid(), ProjectTerminalAction::Input(bytes));
                    } else if !key.modifiers.control && !key.modifiers.alt {
                        if let Some(ch) = key.key_code.to_char(key.modifiers.shift) {
                            cx.widget_action(
                                self.widget_uid(),
                                ProjectTerminalAction::Input(ch.to_string().into_bytes()),
                            );
                        }
                    }
                }
                Event::TextInput(input)
                    if input.was_paste
                        || input.input.chars().any(|character| !character.is_ascii()) =>
                {
                    cx.widget_action(
                        self.widget_uid(),
                        ProjectTerminalAction::Input(input.input.as_bytes().to_vec()),
                    );
                }
                _ => {}
            }
        }
        self.view.handle_event(cx, event, scope);
        match event {
            Event::MouseDown(pointer)
                if self.expanded
                    && pointer.button.is_primary()
                    && self
                        .view
                        .view(cx, ids!(terminal_resize_handle))
                        .area()
                        .rect(cx)
                        .contains(pointer.abs) =>
            {
                self.resizing = true;
                self.resize_start_y = pointer.abs.y;
                self.resize_start_height = self.terminal_height.max(250.0);
                cx.set_cursor(MouseCursor::RowResize);
            }
            Event::MouseMove(pointer) if self.resizing => {
                self.set_terminal_height(
                    cx,
                    (self.resize_start_height + self.resize_start_y - pointer.abs.y)
                        .clamp(160.0, 520.0),
                );
                cx.set_cursor(MouseCursor::RowResize);
            }
            Event::MouseMove(pointer)
                if self.expanded
                    && self
                        .view
                        .view(cx, ids!(terminal_resize_handle))
                        .area()
                        .rect(cx)
                        .contains(pointer.abs) =>
            {
                cx.set_cursor(MouseCursor::RowResize);
            }
            Event::MouseUp(pointer) if pointer.button.is_primary() => self.resizing = false,
            _ => {}
        }
        if self.expanded {
            if let Event::MouseDown(pointer) = event {
                let body = self.view.view(cx, ids!(terminal_body)).area().rect(cx);
                if pointer.button.is_primary() && body.contains(pointer.abs) {
                    self.view.button(cx, ids!(terminal_toggle)).set_key_focus(cx);
                }
            }
        }
        let focused = self.expanded && self.view.button(cx, ids!(terminal_toggle)).key_focus(cx);
        if focused != self.terminal_focused {
            self.terminal_focused = focused;
            self.cursor_blink_on = focused;
            self.cursor_last_blink = 0.0;
            self.render_output(cx);
            if focused {
                self.cursor_next_frame = cx.new_next_frame();
            }
        }
        if let Some(frame) = self.cursor_next_frame.is_event(event) {
            if self.terminal_focused {
                if frame.time - self.cursor_last_blink >= 0.45 {
                    self.cursor_blink_on = !self.cursor_blink_on;
                    self.cursor_last_blink = frame.time;
                    self.render_output(cx);
                }
                self.cursor_next_frame = cx.new_next_frame();
            }
        }
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(terminal_toggle)).clicked(actions) {
                self.toggle(cx);
            }
            if self.view.button(cx, ids!(terminal_new)).clicked(actions) {
                cx.widget_action(self.widget_uid(), ProjectTerminalAction::New);
            }
            for index in 0..MAX_VISIBLE_TERMINALS {
                if self.view.button(cx, tab_id(index)).clicked(actions) {
                    cx.widget_action(self.widget_uid(), ProjectTerminalAction::Select(index));
                }
                if self.view.button(cx, close_id(index)).clicked(actions) {
                    cx.widget_action(self.widget_uid(), ProjectTerminalAction::Close(index));
                }
            }
        }
    }
}

impl ProjectTerminal {
    fn set_terminal_height(&mut self, cx: &mut Cx, height: f64) {
        self.terminal_height = height;
        if let Some(mut body) = self.view.view(cx, ids!(terminal_body)).borrow_mut() {
            body.walk.height = Size::Fixed(height);
            body.redraw(cx);
        }
        cx.redraw_all();
    }

    pub fn toggle(&mut self, cx: &mut Cx) {
        self.expanded = !self.expanded;
        self.view
            .view(cx, ids!(terminal_body))
            .set_visible(cx, self.expanded);
        self.view
            .view(cx, ids!(terminal_resize_handle))
            .set_visible(cx, self.expanded);
        if self.expanded {
            self.focus_next_frame = cx.new_next_frame();
            self.view.button(cx, ids!(terminal_toggle)).set_key_focus(cx);
        }
        cx.widget_action(self.widget_uid(), ProjectTerminalAction::LayoutChanged);
        self.view.redraw(cx);
    }

    fn render_output(&mut self, cx: &mut Cx) {
        let cursor = if self.terminal_focused && self.cursor_blink_on {
            "▌"
        } else {
            ""
        };
        let display = self.output.replace('\u{e000}', cursor);
        self.view.label(cx, ids!(terminal_output)).set_text(cx, &display);
        self.view.redraw(cx);
    }
}

fn encode_key(event: &KeyEvent) -> Option<Vec<u8>> {
    if event.modifiers.control {
        if let Some(ch) = event.key_code.to_char(false) {
            let byte = ch.to_ascii_lowercase() as u8;
            if byte.is_ascii_lowercase() {
                return Some(vec![byte - b'a' + 1]);
            }
        }
    }
    let key = match event.key_code {
        KeyCode::ReturnKey | KeyCode::NumpadEnter => TerminalKeyCode::Return,
        KeyCode::Tab => TerminalKeyCode::Tab,
        KeyCode::Backspace => TerminalKeyCode::Backspace,
        KeyCode::Escape => TerminalKeyCode::Escape,
        KeyCode::Delete => TerminalKeyCode::Delete,
        KeyCode::ArrowUp => TerminalKeyCode::Up,
        KeyCode::ArrowDown => TerminalKeyCode::Down,
        KeyCode::ArrowLeft => TerminalKeyCode::Left,
        KeyCode::ArrowRight => TerminalKeyCode::Right,
        KeyCode::Home => TerminalKeyCode::Home,
        KeyCode::End => TerminalKeyCode::End,
        KeyCode::PageUp => TerminalKeyCode::PageUp,
        KeyCode::PageDown => TerminalKeyCode::PageDown,
        KeyCode::Insert => TerminalKeyCode::Insert,
        KeyCode::F1 => TerminalKeyCode::F1,
        KeyCode::F2 => TerminalKeyCode::F2,
        KeyCode::F3 => TerminalKeyCode::F3,
        KeyCode::F4 => TerminalKeyCode::F4,
        KeyCode::F5 => TerminalKeyCode::F5,
        KeyCode::F6 => TerminalKeyCode::F6,
        KeyCode::F7 => TerminalKeyCode::F7,
        KeyCode::F8 => TerminalKeyCode::F8,
        KeyCode::F9 => TerminalKeyCode::F9,
        KeyCode::F10 => TerminalKeyCode::F10,
        KeyCode::F11 => TerminalKeyCode::F11,
        KeyCode::F12 => TerminalKeyCode::F12,
        _ => TerminalKeyCode::None,
    };
    if key == TerminalKeyCode::None {
        return None;
    }
    Terminal::new(1, 1).encode_key(
        key,
        "",
        event.modifiers.shift,
        event.modifiers.control,
        event.modifiers.alt,
    )
}

impl ProjectTerminalRef {
    pub fn actions(&self, actions: &Actions) -> Vec<ProjectTerminalAction> {
        actions
            .filter_widget_actions_cast::<ProjectTerminalAction>(self.widget_uid())
            .collect()
    }

    pub fn set_project(&self, _cx: &mut Cx, _name: &str) {}

    pub fn toggle(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.toggle(cx);
        }
    }

    pub fn set_terminals(
        &self,
        cx: &mut Cx,
        names: &[String],
        active: Option<usize>,
        output: &str,
    ) {
        for index in 0..MAX_VISIBLE_TERMINALS {
            let visible = index < names.len();
            self.view(cx, slot_id(index)).set_visible(cx, visible);
            if visible {
                let prefix = if active == Some(index) { "● " } else { "" };
                self.button(cx, tab_id(index))
                    .set_text(cx, &format!("{prefix}{}", names[index]));
            }
        }
        self.button(cx, ids!(terminal_new))
            .set_visible(cx, names.len() < MAX_VISIBLE_TERMINALS);
        if let Some(mut inner) = self.borrow_mut() {
            inner.output.clear();
            inner.output.push_str(output);
            inner.render_output(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_core_parses_ansi_into_screen_cells() {
        let mut terminal = Terminal::new(12, 2);
        terminal.process_bytes(b"hello\x1b[31mred");

        let screen = terminal.screen();
        let text: String = screen.grid.row_slice(0).iter().map(|cell| cell.codepoint).collect();
        assert_eq!(text.trim_end(), "hellored");
        assert_ne!(screen.grid.cell(5, 0).style.fg, Default::default());
    }
}
