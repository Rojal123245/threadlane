//! ProjectHeader component for sidebar project tree items.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ProjectHeaderBase = #(ProjectHeader::register_widget(vm)) {
        width: Fill
        height: 36.0
        flow: Right
        spacing: 0
        align: Align{y: 0.5}
        margin: Inset{left: 3 right: 3 top: 4 bottom: 2}
        padding: Inset{left: 8 top: 4 right: 4 bottom: 4}
        draw_bg +: {
            hover: instance(0.0)
            tree_top: instance(0.0)
            tree_color: uniform(theme.color_card)
            color: theme.color_transparent
            color_hover: theme.color_card
            border_color: uniform(theme.color_transparent)
            border_size: uniform(0.0)
            border_radius: 8.0

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(
                    self.border_size
                    self.border_size
                    self.rect_size.x - self.border_size * 2.0
                    self.rect_size.y - self.border_size * 2.0
                    max(1.0 self.border_radius)
                )
                sdf.fill_keep(mix(self.color, self.color_hover, self.hover))
                sdf.stroke(self.border_color, self.border_size)

                let tree_x = 16.0
                let tree_start = self.rect_size.y * 0.5 + 8.0
                if self.tree_top > 0.5 {
                    sdf.rect(tree_x, 0.0, 1.0, max(0.0, tree_start))
                    sdf.fill(self.tree_color)
                }
                sdf.rect(tree_x, tree_start, 1.0, max(0.0, self.rect_size.y - tree_start))
                sdf.fill(self.tree_color)
                return sdf.result
            }
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState {
                    apply: {draw_bg: {hover: 0.0}}
                }
                on: AnimatorState {
                    apply: {draw_bg: {hover: 1.0}}
                }
            }
        }
        project_toggle_surface := View {
            width: Fill
            height: Fill
            cursor: MouseCursor.Hand
            flow: Right
            spacing: 8
            align: Align{y: 0.5}
            folder_icon := Icon {
                width: 16
                height: 16
                icon_walk: Walk{width: 14 height: 14}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/folder.svg")
                    color: theme.color_primary
                }
            }
            name_lbl := mod.components.ClippedLabel {
                height: 18
                draw_text +: {
                    color: theme.color_foreground
                    text_style: theme.font_bold { font_size: 10.5 }
                }
            }
        }
        project_actions_slot := View {
            width: 48
            height: 22
            flow: Right
            spacing: 4
            detach_project_btn := mod.components.IconButton {
                width: 22
                height: 22
                text: "×"
                draw_text +: {
                    color: theme.color_transparent
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                    text_style +: { font_size: 11.0 }
                }
                draw_bg +: {
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
            }
            new_project_session_btn := mod.components.SidebarComposeButton {
                visible: true
                draw_icon +: {
                    color: theme.color_transparent
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
                draw_bg +: {
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum ProjectHeaderAction {
    Toggle,
    NewSession,
    Detach,
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ProjectHeader {
    #[deref]
    view: View,
    #[rust]
    actions_painted: bool,
}

impl ProjectHeader {
    fn set_actions_painted(&mut self, cx: &mut Cx, painted: bool) {
        if self.actions_painted == painted {
            return;
        }
        self.actions_painted = painted;

        let mut detach = self.view.widget(cx, ids!(detach_project_btn));
        let mut compose = self.view.widget(cx, ids!(new_project_session_btn));
        if painted {
            script_apply_eval!(cx, detach, {
                use mod.prelude.widgets.*
                draw_text +: {
                    color: theme.color_muted_foreground
                    color_hover: theme.color_destructive
                    color_focus: theme.color_destructive
                    color_down: theme.color_destructive
                }
                draw_bg +: {
                    color_hover: theme.color_card
                    color_focus: theme.color_card
                    color_down: theme.color_card
                }
            });
            script_apply_eval!(cx, compose, {
                use mod.prelude.widgets.*
                draw_icon +: {
                    color: theme.color_primary
                    color_hover: theme.color_primary
                    color_focus: theme.color_primary
                    color_down: theme.color_primary_foreground
                }
                draw_bg +: {
                    color_hover: theme.color_secondary
                    color_focus: theme.color_secondary
                    color_down: theme.color_input
                }
            });
        } else {
            script_apply_eval!(cx, detach, {
                use mod.prelude.widgets.*
                draw_text +: {
                    color: theme.color_transparent
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
                draw_bg +: {
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
            });
            script_apply_eval!(cx, compose, {
                use mod.prelude.widgets.*
                draw_icon +: {
                    color: theme.color_transparent
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
                draw_bg +: {
                    color_hover: theme.color_transparent
                    color_focus: theme.color_transparent
                    color_down: theme.color_transparent
                }
            });
        }
        detach.redraw(cx);
        compose.redraw(cx);
    }
}

impl Widget for ProjectHeader {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        match event {
            Event::MouseMove(event) => {
                self.set_actions_painted(cx, self.view.area().clipped_rect(cx).contains(event.abs));
            }
            Event::MouseLeave(_) => self.set_actions_painted(cx, false),
            _ => {}
        }

        let Event::Actions(actions) = event else {
            return;
        };
        let uid = self.widget_uid();
        if self
            .view
            .button(cx, ids!(detach_project_btn))
            .clicked(actions)
        {
            cx.widget_action(uid, ProjectHeaderAction::Detach);
        } else if self
            .view
            .button(cx, ids!(new_project_session_btn))
            .clicked(actions)
        {
            cx.widget_action(uid, ProjectHeaderAction::NewSession);
        } else if self
            .view
            .view(cx, ids!(project_toggle_surface))
            .finger_up(actions)
            .is_some_and(|event| event.is_over && event.is_primary_hit() && event.was_tap())
        {
            cx.widget_action(uid, ProjectHeaderAction::Toggle);
        }
    }
}
