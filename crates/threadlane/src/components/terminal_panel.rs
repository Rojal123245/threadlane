//! Compact, expandable project terminal surface with project-scoped tabs.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    mod.components.ProjectTerminalBase = #(ProjectTerminal::register_widget(vm))

    mod.components.TerminalTabButton = mod.widgets.Button {
        width: Fit height: 28 padding: Inset{left: 9 right: 9} spacing: 0
        draw_text +: { color: #xaeb8c5 color_hover: #xe4e9ef color_focus: #xe4e9ef color_down: #xffffff text_style: theme.font_code {font_size: 9.0} }
        draw_bg +: {
            color: #x23262b color_hover: #x2a2e34 color_focus: #x2a2e34 color_down: #x30353d
            border_color: #x30343a border_color_hover: #x414750 border_color_focus: #x414750 border_color_down: #x4b535e
            border_size: 1.0 border_radius: 6.0
        }
    }

    mod.components.TerminalIconButton = mod.widgets.Button {
        width: 28 height: 28 padding: 0 spacing: 0 text: "" align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 12 height: 12}
        draw_icon +: { color: #x8c98a8 color_hover: #xdde3ea color_focus: #xdde3ea color_down: #xffffff }
        draw_bg +: {
            color: #x00000000 color_hover: #x292e35 color_focus: #x292e35 color_down: #x343a43
            border_color: #x00000000 border_color_hover: #x00000000 border_color_focus: #x00000000 border_color_down: #x00000000 border_radius: 5.0
        }
    }

    mod.components.ProjectTerminal = set_type_default() do mod.components.ProjectTerminalBase {
        width: Fill height: Fit flow: Down spacing: 4

        terminal_header := RoundedView {
            width: Fill height: 30 flow: Right padding: Inset{left: 5 right: 5} spacing: 5 align: Align{y: 0.5}
            draw_bg +: { color: #x1d2026 border_color: #x343c48 border_size: 1.0 border_radius: 7.0 }
            terminal_toggle := mod.components.TerminalIconButton {
                draw_icon +: { svg: crate_resource("self:resources/icons/terminal.svg") }
            }
            terminal_title := Label {
                width: Fit height: 30 padding: 0 align: Align{y: 0.5} text: "Terminal"
                draw_text +: { color: #x9da9b8 text_style: theme.font_bold {font_size: 9.0} }
            }
            terminal_project := Label {
                width: Fill height: 30 padding: 0 align: Align{y: 0.5} text: ""
                draw_text +: { color: #x657386 text_style +: {font_size: 8.5} }
            }
        }

        terminal_body := RoundedView {
            width: Fill height: 300 visible: false flow: Down
            draw_bg +: { color: #x151719 border_color: #x343c48 border_size: 1.0 border_radius: 8.0 }

            terminal_tabs := View {
                width: Fill height: 38 flow: Right padding: Inset{left: 8 top: 5 right: 8 bottom: 5} spacing: 4 align: Align{y: 0.5}
                tab_slot_0 := View {width: Fit height: 28 flow: Right spacing: 1 visible: false tab_0 := mod.components.TerminalTabButton{text: ""} close_0 := mod.components.TerminalIconButton{width: 22 draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_1 := View {width: Fit height: 28 flow: Right spacing: 1 visible: false tab_1 := mod.components.TerminalTabButton{text: ""} close_1 := mod.components.TerminalIconButton{width: 22 draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_2 := View {width: Fit height: 28 flow: Right spacing: 1 visible: false tab_2 := mod.components.TerminalTabButton{text: ""} close_2 := mod.components.TerminalIconButton{width: 22 draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_3 := View {width: Fit height: 28 flow: Right spacing: 1 visible: false tab_3 := mod.components.TerminalTabButton{text: ""} close_3 := mod.components.TerminalIconButton{width: 22 draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_4 := View {width: Fit height: 28 flow: Right spacing: 1 visible: false tab_4 := mod.components.TerminalTabButton{text: ""} close_4 := mod.components.TerminalIconButton{width: 22 draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                tab_slot_5 := View {width: Fit height: 28 flow: Right spacing: 1 visible: false tab_5 := mod.components.TerminalTabButton{text: ""} close_5 := mod.components.TerminalIconButton{width: 22 draw_icon +: {svg: crate_resource("self:resources/icons/close.svg")}}}
                terminal_new := mod.components.TerminalIconButton { draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")} }
            }

            terminal_rule := View {width: Fill height: 1 show_bg: true draw_bg +: {color: #x2b3036}}
            terminal_content := View {
                width: Fill height: Fill flow: Down padding: Inset{left: 11 top: 8 right: 11 bottom: 9} spacing: 6
                terminal_scroll := ScrollYView {
                    width: Fill height: Fill
                    terminal_output := Label {
                        width: Fill height: Fit text: ""
                        draw_text +: {color: #xd2d7dd text_style: theme.font_code {font_size: 9.5 line_spacing: 1.3}}
                    }
                }
                terminal_input := TextInput {
                    width: Fill height: 28 empty_text: "Enter a command…" is_multiline: false
                    draw_bg +: {
                        color: #x1d2024 color_empty: #x1d2024 color_hover: #x22272d color_focus: #x22272d color_down: #x22272d
                        border_color: #x30363d border_color_empty: #x30363d border_color_hover: #x46505c border_color_focus: #x4a6f9e border_color_down: #x4a6f9e border_size: 1.0 border_radius: 5.0
                    }
                    draw_text +: {color: #xe2e7ed color_empty: #x758296 text_style: theme.font_code {font_size: 9.0}}
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
    Run(String),
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
}

impl Widget for ProjectTerminal {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(terminal_toggle)).clicked(actions) {
                self.expanded = !self.expanded;
                self.view
                    .view(cx, ids!(terminal_body))
                    .set_visible(cx, self.expanded);
                if self.expanded {
                    self.view
                        .text_input(cx, ids!(terminal_input))
                        .set_key_focus(cx);
                }
                self.view.redraw(cx);
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
            let input = self.view.text_input(cx, ids!(terminal_input));
            if input.returned(actions).is_some() {
                let command = input.text();
                if !command.trim().is_empty() {
                    input.set_text(cx, "");
                    cx.widget_action(self.widget_uid(), ProjectTerminalAction::Run(command));
                }
            }
        }
    }
}

impl ProjectTerminalRef {
    pub fn actions(&self, actions: &Actions) -> Vec<ProjectTerminalAction> {
        actions
            .filter_widget_actions_cast::<ProjectTerminalAction>(self.widget_uid())
            .collect()
    }

    pub fn set_project(&self, cx: &mut Cx, name: &str) {
        self.label(cx, ids!(terminal_project)).set_text(cx, name);
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
                let prefix = if active == Some(index) { "› " } else { "" };
                self.button(cx, tab_id(index))
                    .set_text(cx, &format!("{prefix}{}", names[index]));
            }
        }
        self.button(cx, ids!(terminal_new))
            .set_visible(cx, names.len() < MAX_VISIBLE_TERMINALS);
        self.label(cx, ids!(terminal_output)).set_text(cx, output);
        self.redraw(cx);
    }
}
