use makepad_widgets::*;

#[derive(Script, ScriptHook, Widget)]
pub struct GitDiffView {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    lines: Vec<String>,
}

script_mod! {
    use mod.prelude.widgets.*

    mod.components.GitDiffViewBase = #(GitDiffView::register_widget(vm))

    mod.components.GitDiffView = set_type_default() do mod.components.GitDiffViewBase {
        width: Fill
        height: Fill
        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true
            scroll_bar: mod.widgets.ScrollBar {}

            Line := Label {
                width: Fill
                height: Fit
                padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                draw_text +: {
                    color: theme.color_muted_foreground
                    text_style: theme.font_code { font_size: 7.5 }
                }
            }
            Add := Label {
                width: Fill height: Fit padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                draw_text +: { color: theme.color_success text_style: theme.font_code { font_size: 7.5 } }
            }
            Remove := Label {
                width: Fill height: Fit padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                draw_text +: { color: theme.color_destructive text_style: theme.font_code { font_size: 7.5 } }
            }
            Hunk := Label {
                width: Fill height: Fit padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                draw_text +: { color: theme.color_primary text_style: theme.font_code { font_size: 7.5 } }
            }
            Header := Label {
                width: Fill height: Fit padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                draw_text +: { color: theme.color_foreground text_style: theme.font_code { font_size: 7.5 } }
            }
        }
    }
}

impl GitDiffView {
    pub fn set_text(&mut self, cx: &mut Cx, text: &str) {
        self.lines = text.lines().map(str::to_owned).collect();
        self.view
            .portal_list(cx, ids!(list))
            .set_first_id_and_scroll(0, 0.0);
        self.view.redraw(cx);
    }
}

impl Widget for GitDiffView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, self.lines.len());
                while let Some(index) = list.next_visible_item(cx) {
                    let Some(line) = self.lines.get(index) else {
                        continue;
                    };
                    let template = if line.starts_with("+++") || line.starts_with("+") {
                        id!(Add)
                    } else if line.starts_with("---") || line.starts_with("-") {
                        id!(Remove)
                    } else if line.starts_with("@@") {
                        id!(Hunk)
                    } else if line.starts_with('#') {
                        id!(Header)
                    } else {
                        id!(Line)
                    };
                    let item = list.item(cx, index, template);
                    item.set_text(cx, line);
                    item.draw_all_unscoped(cx);
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}
