use makepad_widgets::*;
use std::collections::HashSet;

use crate::git::GitFile;

#[derive(Clone, Copy, Debug)]
enum GitChangesRow {
    File { index: usize },
}

#[derive(Clone, Debug, Default)]
pub enum GitChangesAction {
    Open(String),
    SelectionChanged,
    #[default]
    None,
}

script_mod! {
    use mod.prelude.widgets.*

    mod.components.GitChangesBase = #(GitChanges::register_widget(vm))

    mod.components.GitChanges = set_type_default() do mod.components.GitChangesBase {
        width: Fill
        height: Fill
        flow: Down
        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true
            scroll_bar: mod.widgets.ScrollBar {}

            Empty := View {
                width: Fill
                height: 42
                align: Align{x: 0.5 y: 0.5}
                empty_lbl := Label {
                    text: "Working tree clean"
                    draw_text +: {
                        color: theme.color_muted_foreground
                        text_style: theme.font_regular { font_size: 9.0 }
                    }
                }
            }

            File := View {
                width: Fill
                height: 28
                flow: Right
                spacing: 2
                align: Align{y: 0.5}
                padding: Inset{left: 0 right: 0}

                select_btn := Button {
                    width: 24
                    height: 28
                    padding: 0
                    spacing: 0
                    text: "[ ]"
                    align: Align{x: 0.5 y: 0.5}
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_card
                        color_down: theme.color_primary_tint
                        border_color: theme.color_transparent
                        border_size: 0.0
                        border_radius: 4.0
                    }
                    draw_text +: {
                        color: theme.color_muted_foreground
                        color_hover: theme.color_foreground
                        color_down: theme.color_primary_foreground
                        text_style: theme.font_code { font_size: 9.0 }
                    }
                }
                status_lbl := Label {
                    width: 14
                    height: 28
                    padding: 0
                    margin: 0
                    align: Align{x: 0.5 y: 0.5}
                    draw_text +: {
                        color: theme.color_primary
                        staged_color: uniform(theme.color_success)
                        untracked_color: uniform(theme.color_warning)
                        status_staged: instance(0.0)
                        status_untracked: instance(0.0)
                        get_color: fn() {
                            return self.color
                                .mix(self.staged_color, self.status_staged)
                                .mix(self.untracked_color, self.status_untracked)
                        }
                        text_style: theme.font_code { font_size: 8.5 }
                    }
                }
                path_btn := Button {
                    width: Fill
                    height: 28
                    padding: Inset{left: 4 right: 6}
                    spacing: 0
                    align: Align{x: 0.0 y: 0.5}
                    text: ""
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_card
                        color_down: theme.color_secondary
                        border_color: theme.color_transparent
                        border_size: 0.0
                        border_radius: 4.0
                    }
                    draw_text +: {
                        color: theme.color_foreground
                        color_hover: theme.color_primary_foreground
                        text_style: theme.font_code { font_size: 8.5 }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct GitChanges {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    files: Vec<GitFile>,
    #[rust]
    rows: Vec<GitChangesRow>,
    #[rust]
    selected: HashSet<String>,
}

impl GitChanges {
    pub fn set_files(&mut self, cx: &mut Cx, files: Vec<GitFile>) {
        if self.files == files {
            return;
        }
        for file in &files {
            if !self.files.iter().any(|f| f.path == file.path) {
                self.selected.insert(file.path.clone());
            }
        }
        self.files = files;
        self.selected
            .retain(|path| self.files.iter().any(|file| &file.path == path));
        self.rebuild_rows();
        self.view.redraw(cx);
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for index in 0..self.files.len() {
            self.rows.push(GitChangesRow::File { index });
        }
    }

    pub fn selected_files(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| self.selected.contains(&file.path))
            .map(|file| file.path.clone())
            .collect()
    }

    pub fn all_files(&self) -> Vec<String> {
        self.files.iter().map(|file| file.path.clone()).collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn toggle_all(&mut self, cx: &mut Cx) {
        if self.selected.len() == self.files.len() {
            self.selected.clear();
        } else {
            self.selected = self.files.iter().map(|file| file.path.clone()).collect();
        }
        self.view.redraw(cx);
    }

    pub fn clear_selection(&mut self, cx: &mut Cx) {
        if !self.selected.is_empty() {
            self.selected.clear();
            self.view.redraw(cx);
        }
    }
}

impl Widget for GitChanges {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.as_portal_list().borrow_mut() {
                if self.files.is_empty() {
                    list.set_item_range(cx, 0, 0);
                    let item = list.item(cx, 0, id!(Empty));
                    item.draw_all_unscoped(cx);
                    continue;
                }
                list.set_item_range(cx, 0, self.rows.len());
                while let Some(row_index) = list.next_visible_item(cx) {
                    match self.rows.get(row_index).copied() {
                        Some(GitChangesRow::File { index: file_index }) => {
                            let Some(file) = self.files.get(file_index) else {
                                continue;
                            };
                            let row = list.item(cx, row_index, id!(File));
                            let target_check = if self.selected.contains(&file.path) {
                                "[x]"
                            } else {
                                "[ ]"
                            };
                            let select_btn = row.button(cx, ids!(select_btn));
                            if select_btn.text() != target_check {
                                select_btn.set_text(cx, target_check);
                            }
                            let status = file.status_char();
                            let status_label = row.label(cx, ids!(status_lbl));
                            let status_str = status.to_string();
                            if status_label.text() != status_str {
                                status_label.set_text(cx, &status_str);
                            }
                            if let Some(mut status_label) = status_label.borrow_mut() {
                                status_label.draw_text.set_uniform(
                                    cx,
                                    id!(status_staged),
                                    &[if file.staged { 1.0 } else { 0.0 }],
                                );
                                status_label.draw_text.set_uniform(
                                    cx,
                                    id!(status_untracked),
                                    &[if status == '?' { 1.0 } else { 0.0 }],
                                );
                            }
                            let path_btn = row.button(cx, ids!(path_btn));
                            if path_btn.text() != file.path {
                                path_btn.set_text(cx, &file.path);
                            }
                            row.draw_all_unscoped(cx);
                        }
                        None => {}
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let uid = self.widget_uid();
            let list = self.view.portal_list(cx, ids!(list));
            for (index, row) in list.items_with_actions(actions) {
                let Some(GitChangesRow::File { index: file_index }) = self.rows.get(index).copied()
                else {
                    continue;
                };
                let Some(file) = self.files.get(file_index) else {
                    continue;
                };
                if row.button(cx, ids!(select_btn)).clicked(actions) {
                    if !self.selected.insert(file.path.clone()) {
                        self.selected.remove(&file.path);
                    }
                    cx.widget_action(uid, GitChangesAction::SelectionChanged);
                    self.view.redraw(cx);
                } else if row.button(cx, ids!(path_btn)).clicked(actions) {
                    cx.widget_action(uid, GitChangesAction::Open(file.path.clone()));
                }
            }
        }
    }
}
