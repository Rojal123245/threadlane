//! Compact, expandable project terminal surface.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    mod.components.ProjectTerminalBase = #(ProjectTerminal::register_widget(vm))

    mod.components.ProjectTerminal = set_type_default() do mod.components.ProjectTerminalBase {
        width: Fill
        height: Fit
        flow: Down

        terminal_header := RoundedView {
            width: Fill
            height: 30
            flow: Right
            padding: Inset{left: 8 right: 8}
            spacing: 7
            align: Align{y: 0.5}
            draw_bg +: { color: #x1f232b border_color: #x343c48 border_size: 1.0 border_radius: 7.0 }

            terminal_toggle := Button {
                width: 28
                height: 26
                padding: 0
                spacing: 0
                text: ""
                align: Align{x: 0.5 y: 0.5}
                icon_walk: Walk{width: 14 height: 14}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/terminal.svg")
                    color: #x93a1b2
                    color_hover: #xdde3ea
                    color_focus: #xdde3ea
                    color_down: #xffffff
                }
                draw_bg +: {
                    color: #x00000000 color_hover: #x29303a color_focus: #x29303a color_down: #x343e4c
                    border_color: #x00000000 border_color_hover: #x00000000 border_color_focus: #x00000000 border_color_down: #x00000000
                    border_radius: 6.0
                }
            }
            terminal_title := Label {
                width: Fit height: 30 padding: 0 align: Align{y: 0.5}
                text: "Terminal"
                draw_text +: { color: #x9da9b8 text_style: theme.font_bold {font_size: 9.0} }
            }
            terminal_project := Label {
                width: Fill height: 30 padding: 0 align: Align{y: 0.5}
                text: ""
                draw_text +: { color: #x657386 text_style +: {font_size: 8.5} }
            }
        }

        terminal_body := RoundedView {
            width: Fill
            height: 230
            visible: false
            flow: Down
            padding: Inset{left: 9 top: 7 right: 9 bottom: 7}
            spacing: 5
            draw_bg +: { color: #x171a20 border_color: #x343c48 border_size: 1.0 border_radius: 7.0 }

            terminal_scroll := ScrollYView {
                width: Fill height: Fill
                terminal_output := Label {
                    width: Fill height: Fit
                    text: "Terminal ready.\n"
                    draw_text +: {
                        color: #xc9d1db
                        text_style: theme.font_code {font_size: 9.0 line_spacing: 1.3}
                        wrap: Word
                    }
                }
            }
            terminal_input := TextInput {
                width: Fill height: 28
                empty_text: "Enter a command…"
                is_multiline: false
                draw_bg +: {
                    color: #x1f232b color_empty: #x1f232b color_hover: #x222832 color_focus: #x222832 color_down: #x222832
                    border_color: #x343c48 border_color_empty: #x343c48 border_color_hover: #x465264 border_color_focus: #x4a6f9e border_color_down: #x4a6f9e
                    border_size: 1.0 border_radius: 5.0
                }
                draw_text +: { color: #xe2e7ed color_empty: #x758296 text_style: theme.font_code {font_size: 9.0} }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum ProjectTerminalAction {
    Run(String),
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
    pub fn command(&self, actions: &Actions) -> Option<String> {
        actions
            .filter_widget_actions_cast::<ProjectTerminalAction>(self.widget_uid())
            .find_map(|action| match action {
                ProjectTerminalAction::Run(command) => Some(command),
                ProjectTerminalAction::None => None,
            })
    }

    pub fn set_project(&self, cx: &mut Cx, name: &str) {
        self.label(cx, ids!(terminal_project)).set_text(cx, name);
    }

    pub fn set_output(&self, cx: &mut Cx, output: &str) {
        self.label(cx, ids!(terminal_output)).set_text(cx, output);
        self.redraw(cx);
    }
}
