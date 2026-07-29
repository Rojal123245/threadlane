//! Shared icon-aware dropdown for model-provider and reasoning-effort selection.

use makepad_widgets::widget::WidgetActionData;
use makepad_widgets::*;
use std::{cell::RefCell, rc::Rc};

fn is_antigravity_model(model: &str) -> bool {
    model.starts_with("antigravity/")
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.components.IconPopupMenuItemBase = #(IconPopupMenuItem::script_component(vm))
    mod.components.IconPopupMenuBase = #(IconPopupMenu::script_component(vm))
    mod.components.IconDropDownBase = #(IconDropDown::register_widget(vm))

    mod.components.IconPopupMenuItem = mod.components.IconPopupMenuItemBase {
        width: Fill
        height: 24
        align: Align{y: 0.5}
        padding: Inset{left: 10 right: 10}
        icon_walk: Walk{width: 13 height: 13 margin: Inset{right: 7}}
        use_provider_icons: true
        draw_openai_icon +: {
            svg: crate_resource("self:resources/icons/openai.svg")
            color: theme.color_foreground
        }
        draw_antigravity_icon +: {
            svg: crate_resource("self:resources/icons/google.svg")
        }
        draw_icon +: {
            svg: crate_resource("self:resources/icons/reasoning.svg")
            color: theme.color_primary
        }
        draw_action_icon +: {
            svg: crate_resource("self:resources/icons/plus.svg")
            color: theme.color_primary
        }
        draw_text +: {
            color: theme.color_foreground
            color_hover: theme.color_primary_foreground
            hover: instance(0.0)
            active: instance(0.0)
            get_color: fn() {
                return self.color.mix(self.color_hover, self.hover) * (1.0 - self.active)
            }
            text_style: theme.font_regular { font_size: 9.5 }
        }
        draw_bg +: {
            color: theme.color_transparent
            color_hover: theme.color_card
            hover: instance(0.0)
            active: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 5.0)
                sdf.fill(self.color.mix(self.color_hover, self.hover) * (1.0 - self.active))
                return sdf.result
            }
        }
        animator: Animator {
            hover: {
                default: @off
                off: AnimatorState {
                    from: {all: Snap}
                    apply: {
                        draw_bg: {hover: 0.0}
                        draw_text: {hover: 0.0}
                    }
                }
                on: AnimatorState {
                    cursor: MouseCursor.Hand
                    from: {all: Snap}
                    apply: {
                        draw_bg: {hover: 1.0}
                        draw_text: {hover: 1.0}
                    }
                }
            }
            active: {
                default: @off
                off: AnimatorState {
                    from: {all: Snap}
                    apply: {
                        draw_bg: {active: 0.0}
                        draw_text: {active: 0.0}
                    }
                }
                on: AnimatorState {
                    from: {all: Snap}
                    apply: {
                        draw_bg: {active: 1.0}
                        draw_text: {active: 1.0}
                    }
                }
            }
        }
    }

    // The selected option is kept last by the app. Its transparent 24px row
    // anchors OnSelected above the trigger, leaving the closed picker visible.
    mod.components.IconPopupMenu = mod.components.IconPopupMenuBase {
        height: Fit
        flow: Down
        padding: Inset{left: 4 top: 4 right: 4 bottom: 0}
        menu_item: mod.components.IconPopupMenuItem {}
        draw_bg +: {
            color: theme.color_background
            border_color: theme.color_input
            connector_color: uniform(theme.color_primary)
            border_size: 1.0
            border_radius: 7.0
            selected_anchor_height: uniform(24.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let visible_height = max(
                    0.0,
                    self.rect_size.y - self.selected_anchor_height
                )
                sdf.box(
                    self.border_size,
                    self.border_size,
                    self.rect_size.x - self.border_size * 2.0,
                    max(0.0, visible_height - self.border_size * 2.0),
                    self.border_radius
                )
                sdf.fill_keep(self.color)
                sdf.stroke(self.border_color, self.border_size)
                sdf.move_to(10.0, visible_height - 1.0)
                sdf.line_to(self.rect_size.x - 10.0, visible_height - 1.0)
                sdf.stroke(self.connector_color, 1.0)
                return sdf.result
            }
        }
    }

    mod.components.IconDropDown = mod.components.IconDropDownBase {
        width: Fill
        height: Fill
        margin: 0
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 10 right: 24}
        icon_walk: Walk{width: 13 height: 13 margin: Inset{right: 7}}
        use_provider_icons: true
        draw_openai_icon +: {
            svg: crate_resource("self:resources/icons/openai.svg")
            color: theme.color_card_foreground
        }
        draw_antigravity_icon +: {
            svg: crate_resource("self:resources/icons/google.svg")
        }
        draw_icon +: {
            svg: crate_resource("self:resources/icons/reasoning.svg")
            color: theme.color_primary
        }
        draw_bg +: {
            hover: instance(0.0)
            focus: instance(0.0)
            down: instance(0.0)
            disabled: instance(0.0)
            color: theme.color_secondary
            color_hover: theme.color_secondary
            color_focus: theme.color_secondary
            color_down: theme.color_input
            border_color: theme.color_border
            border_color_hover: theme.color_input
            border_color_focus: theme.color_primary
            border_color_down: theme.color_primary
            border_size: 1.0
            border_radius: 6.0
            arrow_color: theme.color_muted_foreground
            arrow_color_hover: theme.color_card_foreground
            arrow_color_focus: theme.color_card_foreground
            arrow_color_down: theme.color_primary_foreground
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let fill = self.color
                    .mix(self.color_focus, self.focus)
                    .mix(self.color_hover, self.hover)
                    .mix(self.color_down, self.down * self.hover)
                let stroke = self.border_color
                    .mix(self.border_color_focus, self.focus)
                    .mix(self.border_color_hover, self.hover)
                    .mix(self.border_color_down, self.down * self.hover)
                sdf.box(
                    self.border_size,
                    self.border_size,
                    self.rect_size.x - self.border_size * 2.0,
                    self.rect_size.y - self.border_size * 2.0,
                    self.border_radius
                )
                sdf.fill_keep(fill)
                sdf.stroke(stroke, self.border_size)

                let arrow = self.arrow_color
                    .mix(self.arrow_color_focus, self.focus)
                    .mix(self.arrow_color_hover, self.hover)
                    .mix(self.arrow_color_down, self.down * self.hover)
                let c = vec2(self.rect_size.x - 12.0, self.rect_size.y * 0.5 + 1.0)
                let sz = 2.5
                sdf.move_to(c.x - sz, c.y - sz)
                sdf.line_to(c.x + sz, c.y - sz)
                sdf.line_to(c.x, c.y + sz * 0.25)
                sdf.close_path()
                sdf.fill(arrow)
                return sdf.result
            }
        }
        draw_text +: {
            hover: instance(0.0)
            focus: instance(0.0)
            down: instance(0.0)
            color: theme.color_card_foreground
            color_hover: theme.color_foreground
            color_focus: theme.color_foreground
            color_down: theme.color_primary_foreground
            get_color: fn() {
                return self.color
                    .mix(self.color_focus, self.focus)
                    .mix(self.color_hover, self.hover)
                    .mix(self.color_down, self.down * self.hover)
            }
            text_style: theme.font_regular { font_size: 9.5 }
        }
        selected_item: 0
        animator: Animator {
            disabled: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward{duration: 0.0}}
                    apply: {draw_bg: {disabled: 0.0}}
                }
                on: AnimatorState {
                    from: {all: Forward{duration: 0.2}}
                    apply: {draw_bg: {disabled: 1.0}}
                }
            }
            hover: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward{duration: 0.1}}
                    apply: {
                        draw_bg: {down: 0.0 hover: 0.0}
                        draw_text: {down: 0.0 hover: 0.0}
                    }
                }
                on: AnimatorState {
                    from: {all: Forward{duration: 0.1} down: Forward{duration: 0.01}}
                    apply: {
                        draw_bg: {down: 0.0 hover: 1.0}
                        draw_text: {down: 0.0 hover: 1.0}
                    }
                }
                down: AnimatorState {
                    from: {all: Forward{duration: 0.2}}
                    apply: {
                        draw_bg: {down: 1.0 hover: 1.0}
                        draw_text: {down: 1.0 hover: 1.0}
                    }
                }
            }
            focus: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward{duration: 0.2}}
                    apply: {
                        draw_bg: {focus: 0.0}
                        draw_text: {focus: 0.0}
                    }
                }
                on: AnimatorState {
                    cursor: MouseCursor.Arrow
                    from: {all: Forward{duration: 0.0}}
                    apply: {
                        draw_bg: {focus: 1.0}
                        draw_text: {focus: 1.0}
                    }
                }
            }
        }
    }

    mod.components.ModelDropDown = mod.components.IconDropDown {
        use_provider_icons: true
        popup_menu: mod.components.IconPopupMenu {
            width: 226
            menu_item: mod.components.IconPopupMenuItem {
                use_provider_icons: true
            }
        }
    }

    mod.components.EffortDropDown = mod.components.IconDropDown {
        use_provider_icons: false
        popup_menu: mod.components.IconPopupMenu {
            width: 92
            menu_item: mod.components.IconPopupMenuItem {
                use_provider_icons: false
            }
        }
    }

    mod.components.GitBranchDropDown = mod.components.IconDropDown {
        use_provider_icons: false
        padding: Inset{left: 8 right: 22}
        icon_walk: Walk{width: 14 height: 14 margin: Inset{right: 6}}
        draw_icon +: {
            svg: crate_resource("self:resources/icons/git.svg")
            color: theme.color_primary
            color_hover: theme.color_primary_foreground
            color_focus: theme.color_primary_foreground
            color_down: theme.color_primary_foreground
        }
        popup_menu: mod.components.IconPopupMenu {
            width: 132
            menu_item: mod.components.IconPopupMenuItem {
                use_provider_icons: false
                icon_walk: Walk{width: 14 height: 14 margin: Inset{right: 6}}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/git.svg")
                    color: theme.color_primary
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum IconDropDownAction {
    Select(usize),
    #[default]
    None,
}

#[derive(Clone, Debug, Default, Eq, Hash, Copy, PartialEq, FromLiveId)]
struct IconPopupMenuItemId(LiveId);

#[derive(Script, ScriptHook, Animator)]
struct IconPopupMenuItem {
    #[source]
    source: ScriptObjectRef,
    #[live]
    draw_bg: DrawQuad,
    #[live]
    draw_openai_icon: DrawSvg,
    #[live]
    draw_antigravity_icon: DrawSvg,
    #[live]
    draw_icon: DrawSvg,
    #[live]
    draw_action_icon: DrawSvg,
    #[live]
    use_provider_icons: bool,
    #[live]
    draw_text: DrawText,
    #[live]
    icon_walk: Walk,
    #[layout]
    layout: Layout,
    #[walk]
    walk: Walk,
    #[apply_default]
    animator: Animator,
}

impl IconPopupMenuItem {
    fn draw_item(&mut self, cx: &mut Cx2d, label: &str, is_anchor: bool) {
        self.animator_cut(
            cx,
            if is_anchor {
                ids!(active.on)
            } else {
                ids!(active.off)
            },
        );
        self.draw_bg.begin(cx, self.walk, self.layout);
        if !is_anchor {
            if self.use_provider_icons {
                if is_antigravity_model(label) {
                    self.draw_antigravity_icon.draw_walk(cx, self.icon_walk);
                } else {
                    self.draw_openai_icon.draw_walk(cx, self.icon_walk);
                }
            } else {
                if label == "New branch…" || label == "＋ New branch…" {
                    self.draw_action_icon.draw_walk(cx, self.icon_walk);
                } else {
                    self.draw_icon.draw_walk(cx, self.icon_walk);
                }
            }
            self.draw_text
                .draw_walk(cx, Walk::fit(), Align::default(), label);
        }
        self.draw_bg.end(cx);
    }

    fn handle_event_with(
        &mut self,
        cx: &mut Cx,
        event: &Event,
        sweep_area: Area,
        dispatch_action: &mut dyn FnMut(&mut Cx, IconPopupMenuItemAction),
    ) {
        if self.animator_handle_event(cx, event).must_redraw() {
            self.draw_bg.area().redraw(cx);
        }
        match event.hits_with_options(
            cx,
            self.draw_bg.area(),
            HitOptions::new().with_sweep_area(sweep_area),
        ) {
            Hit::FingerHoverIn(_) => self.animator_play(cx, ids!(hover.on)),
            Hit::FingerHoverOut(_) => self.animator_play(cx, ids!(hover.off)),
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                dispatch_action(cx, IconPopupMenuItemAction::WasSweeped);
                self.animator_play(cx, ids!(hover.on));
            }
            Hit::FingerUp(fe) if fe.is_primary_hit() => {
                if !fe.is_sweep {
                    dispatch_action(cx, IconPopupMenuItemAction::WasSelected);
                } else {
                    self.animator_play(cx, ids!(hover.off));
                }
            }
            _ => {}
        }
    }
}

enum IconPopupMenuItemAction {
    WasSweeped,
    WasSelected,
}

#[derive(Clone, Default)]
enum IconPopupMenuAction {
    WasSweeped,
    WasSelected(IconPopupMenuItemId),
    #[default]
    None,
}

#[derive(Script)]
struct IconPopupMenu {
    #[source]
    source: ScriptObjectRef,
    #[live]
    draw_list: DrawList2d,
    #[live]
    menu_item: ScriptValue,
    #[live]
    draw_bg: DrawQuad,
    #[layout]
    layout: Layout,
    #[walk]
    walk: Walk,
    #[rust]
    menu_items: ComponentMap<IconPopupMenuItemId, IconPopupMenuItem>,
    #[rust]
    init_select_item: Option<IconPopupMenuItemId>,
}

impl ScriptHook for IconPopupMenu {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        scope: &mut Scope,
        _value: ScriptValue,
    ) {
        if !self.menu_item.is_nil() {
            for item in self.menu_items.values_mut() {
                item.script_apply(vm, apply, scope, self.menu_item);
            }
        }
        self.draw_list.redraw(vm.cx_mut());
    }
}

impl IconPopupMenu {
    fn menu_contains_pos(&self, cx: &mut Cx, pos: Vec2d) -> bool {
        self.draw_bg.area().clipped_rect(cx).contains(pos)
    }

    fn begin(&mut self, cx: &mut Cx2d) {
        self.draw_list.begin_overlay_reuse(cx);
        cx.begin_root_turtle(cx.current_pass_size(), Layout::flow_down());
        self.draw_bg.begin(cx, self.walk, self.layout);
    }

    fn draw_item(
        &mut self,
        cx: &mut Cx2d,
        item_id: IconPopupMenuItemId,
        label: &str,
        is_anchor: bool,
    ) {
        let template = self.menu_item;
        let item = self.menu_items.get_or_insert(cx, item_id, |cx| {
            cx.with_vm(|vm| IconPopupMenuItem::script_from_value(vm, template))
        });
        item.draw_item(cx, label, is_anchor);
    }

    fn end(&mut self, cx: &mut Cx2d, shift_area: Area, shift: Vec2d) {
        self.draw_bg.end(cx);
        cx.end_pass_sized_turtle_with_shift(shift_area, shift);
        self.draw_list.end(cx);
        self.menu_items.retain_visible();
        if let Some(selected) = self.init_select_item.take() {
            self.select_item_state(cx, selected);
        }
    }

    fn init_select_item(&mut self, item_id: IconPopupMenuItemId) {
        self.init_select_item = Some(item_id);
    }

    fn select_item_state(&mut self, cx: &mut Cx, selected: IconPopupMenuItemId) {
        for (item_id, item) in self.menu_items.iter_mut() {
            item.animator_cut(
                cx,
                if *item_id == selected {
                    ids!(hover.on)
                } else {
                    ids!(hover.off)
                },
            );
        }
    }

    fn handle_event_with(
        &mut self,
        cx: &mut Cx,
        event: &Event,
        sweep_area: Area,
        dispatch_action: &mut dyn FnMut(&mut Cx, IconPopupMenuAction),
    ) {
        let mut actions = Vec::new();
        for (item_id, item) in self.menu_items.iter_mut() {
            item.handle_event_with(cx, event, sweep_area, &mut |_, action| {
                actions.push((*item_id, action));
            });
        }
        for (item_id, action) in actions {
            self.select_item_state(cx, item_id);
            match action {
                IconPopupMenuItemAction::WasSweeped => {
                    dispatch_action(cx, IconPopupMenuAction::WasSweeped);
                }
                IconPopupMenuItemAction::WasSelected => {
                    dispatch_action(cx, IconPopupMenuAction::WasSelected(item_id));
                }
            }
        }
    }
}

#[derive(Default, Clone)]
struct IconPopupMenuGlobal {
    map: Rc<RefCell<ComponentMap<ScriptValue, IconPopupMenu>>>,
}

#[derive(Script, Widget, Animator)]
pub struct IconDropDown {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[apply_default]
    animator: Animator,
    #[redraw]
    #[live]
    draw_bg: DrawQuad,
    #[live]
    draw_openai_icon: DrawSvg,
    #[live]
    draw_antigravity_icon: DrawSvg,
    #[live]
    draw_icon: DrawSvg,
    #[live]
    use_provider_icons: bool,
    #[live]
    draw_text: DrawText,
    #[live]
    icon_walk: Walk,
    #[walk]
    walk: Walk,
    #[live]
    popup_menu: ScriptValue,
    #[live]
    labels: Vec<String>,
    #[live]
    popup_menu_position: PopupMenuPosition,
    #[rust]
    is_active: bool,
    #[live]
    selected_item: usize,
    #[live(true)]
    visible: bool,
    #[layout]
    layout: Layout,
    #[action_data]
    #[rust]
    action_data: WidgetActionData,
}

impl ScriptHook for IconDropDown {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        _apply: &Apply,
        _scope: &mut Scope,
        _value: ScriptValue,
    ) {
        if self.popup_menu.is_nil() {
            return;
        }
        vm.with_cx_mut(|cx| {
            let global = cx.global::<IconPopupMenuGlobal>().clone();
            let Ok(mut map) = global.map.try_borrow_mut() else {
                return;
            };
            let template = self.popup_menu;
            map.get_or_insert(cx, template, |cx| {
                cx.with_vm(|vm| IconPopupMenu::script_from_value(vm, template))
            });
        });
    }
}

impl IconDropDown {
    fn set_active(&mut self, cx: &mut Cx) {
        self.is_active = true;
        self.draw_bg.redraw(cx);
        let global = cx.global::<IconPopupMenuGlobal>().clone();
        let mut map = global.map.borrow_mut();
        if let Some(menu) = map.get_mut(&self.popup_menu) {
            menu.init_select_item(LiveId(self.selected_item as u64).into());
            cx.sweep_lock(self.draw_bg.area());
        }
    }

    fn set_closed(&mut self, cx: &mut Cx) {
        self.is_active = false;
        self.draw_bg.redraw(cx);
        cx.sweep_unlock(self.draw_bg.area());
    }

    fn draw_drop_down(&mut self, cx: &mut Cx2d, walk: Walk) {
        self.draw_bg.begin(cx, walk, self.layout);
        let label = self
            .labels
            .get(self.selected_item)
            .map(String::as_str)
            .unwrap_or(" ");
        if self.use_provider_icons {
            if is_antigravity_model(label) {
                self.draw_antigravity_icon.draw_walk(cx, self.icon_walk);
            } else {
                self.draw_openai_icon.draw_walk(cx, self.icon_walk);
            }
        } else {
            self.draw_icon.draw_walk(cx, self.icon_walk);
        }
        self.draw_text
            .draw_walk(cx, Walk::fit(), Align::default(), label);
        self.draw_bg.end(cx);
        cx.add_nav_stop(self.draw_bg.area(), NavRole::DropDown, Inset::default());

        if self.is_active && !self.popup_menu.is_nil() {
            let global = cx.global::<IconPopupMenuGlobal>().clone();
            let mut map = global.map.borrow_mut();
            let Some(menu) = map.get_mut(&self.popup_menu) else {
                return;
            };
            menu.begin(cx);
            let mut selected_position = None;
            for (index, label) in self.labels.iter().enumerate() {
                if index == self.selected_item {
                    selected_position = Some(cx.turtle().pos());
                }
                menu.draw_item(
                    cx,
                    LiveId(index as u64).into(),
                    label,
                    index == self.selected_item,
                );
            }
            match self.popup_menu_position {
                PopupMenuPosition::OnSelected => menu.end(
                    cx,
                    self.draw_bg.area(),
                    -selected_position.unwrap_or(dvec2(0.0, 0.0)),
                ),
                PopupMenuPosition::BelowInput => {
                    let area = self.draw_bg.area().rect(cx);
                    menu.end(cx, self.draw_bg.area(), dvec2(0.0, area.size.y));
                }
            }
        }
    }
}

impl Widget for IconDropDown {
    fn set_disabled(&mut self, cx: &mut Cx, disabled: bool) {
        self.animator_toggle(
            cx,
            disabled,
            Animate::Yes,
            ids!(disabled.on),
            ids!(disabled.off),
        );
    }

    fn disabled(&self, cx: &Cx) -> bool {
        self.animator_in_state(cx, ids!(disabled.on))
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if !self.visible {
            return;
        }
        self.animator_handle_event(cx, event);
        let uid = self.widget_uid();

        if self.is_active && !self.popup_menu.is_nil() {
            let global = cx.global::<IconPopupMenuGlobal>().clone();
            let mut map = global.map.borrow_mut();
            if let Some(menu) = map.get_mut(&self.popup_menu) {
                let mut close = false;
                menu.handle_event_with(cx, event, self.draw_bg.area(), &mut |cx, action| {
                    if let IconPopupMenuAction::WasSelected(item_id) = action {
                        self.selected_item = item_id.0 .0 as usize;
                        cx.widget_action_with_data(
                            &self.action_data,
                            uid,
                            IconDropDownAction::Select(self.selected_item),
                        );
                        self.draw_bg.redraw(cx);
                        close = true;
                    }
                });
                if close {
                    self.set_closed(cx);
                } else if let Event::MouseDown(pointer) = event {
                    if !menu.menu_contains_pos(cx, pointer.abs) {
                        self.set_closed(cx);
                        self.animator_play(cx, ids!(hover.off));
                        return;
                    }
                }
            }
        }

        match event.hits_with_sweep_area(cx, self.draw_bg.area(), self.draw_bg.area()) {
            Hit::KeyFocusLost(_) => {
                self.animator_play(cx, ids!(focus.off));
                self.set_closed(cx);
                self.animator_play(cx, ids!(hover.off));
            }
            Hit::KeyFocus(_) => self.animator_play(cx, ids!(focus.on)),
            Hit::KeyDown(key) => match key.key_code {
                KeyCode::ArrowUp if self.selected_item > 0 => {
                    self.selected_item -= 1;
                    cx.widget_action_with_data(
                        &self.action_data,
                        uid,
                        IconDropDownAction::Select(self.selected_item),
                    );
                    self.set_closed(cx);
                    self.draw_bg.redraw(cx);
                }
                KeyCode::ArrowDown if self.selected_item.saturating_add(1) < self.labels.len() => {
                    self.selected_item += 1;
                    cx.widget_action_with_data(
                        &self.action_data,
                        uid,
                        IconDropDownAction::Select(self.selected_item),
                    );
                    self.set_closed(cx);
                    self.draw_bg.redraw(cx);
                }
                _ => {}
            },
            Hit::FingerDown(pointer) if pointer.is_primary_hit() => {
                if self.animator_in_state(cx, ids!(disabled.off)) {
                    cx.set_key_focus(self.draw_bg.area());
                    self.animator_play(cx, ids!(hover.down));
                    self.set_active(cx);
                }
            }
            Hit::FingerHoverIn(_) => {
                cx.set_cursor(MouseCursor::Hand);
                self.animator_play(cx, ids!(hover.on));
            }
            Hit::FingerHoverOut(_) => self.animator_play(cx, ids!(hover.off)),
            Hit::FingerUp(pointer) if pointer.is_primary_hit() => {
                self.animator_play(
                    cx,
                    if pointer.is_over && pointer.device.has_hovers() {
                        ids!(hover.on)
                    } else {
                        ids!(hover.off)
                    },
                );
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible {
            return DrawStep::done();
        }
        self.draw_drop_down(cx, walk);
        DrawStep::done()
    }
}

impl IconDropDownRef {
    pub fn set_visible(&self, cx: &mut Cx, visible: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.visible = visible;
            inner.draw_bg.redraw(cx);
        }
    }

    pub fn set_labels(&self, cx: &mut Cx, labels: Vec<String>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.labels = labels;
            inner.selected_item = inner
                .selected_item
                .min(inner.labels.len().saturating_sub(1));
            inner.draw_bg.redraw(cx);
        }
    }

    pub fn set_selected_item(&self, cx: &mut Cx, item: usize) {
        if let Some(mut inner) = self.borrow_mut() {
            let selected = item.min(inner.labels.len().saturating_sub(1));
            if selected != inner.selected_item {
                inner.selected_item = selected;
                inner.draw_bg.redraw(cx);
            }
        }
    }

    pub fn selected_label(&self) -> String {
        self.borrow()
            .and_then(|inner| inner.labels.get(inner.selected_item).cloned())
            .unwrap_or_default()
    }

    pub fn selected(&self, actions: &Actions) -> Option<usize> {
        let action = actions.find_widget_action(self.widget_uid())?;
        match action.cast() {
            IconDropDownAction::Select(index) => Some(index),
            IconDropDownAction::None => None,
        }
    }
}
