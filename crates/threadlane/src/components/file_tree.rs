use makepad_widgets::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTreeNode {
    pub rel_path: String,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub parent_rel_path: Option<String>,
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub enum FileTreeAction {
    FileClicked(String),
    FolderToggled(String),
    #[default]
    None,
}

script_mod! {
    use mod.prelude.widgets.*

    mod.components.FileTreeBase = #(FileTree::register_widget(vm))

    mod.components.FileTree = set_type_default() do mod.components.FileTreeBase {
        width: Fill
        height: Fill
        flow: Down

        header := View {
            width: Fill
            height: 26
            flow: Right
            align: Align{y: 0.5}
            padding: Inset{left: 8 right: 8}
            title_lbl := mod.components.ClippedLabel {
                width: Fill
                height: 16
                text: "WORKSPACE FILES"
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_muted_foreground
                    text_style: theme.font_bold { font_size: 7.5 }
                }
            }
            count_lbl := mod.components.ClippedLabel {
                width: Fit
                height: 16
                text: "0 items"
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_muted_foreground
                    text_style: theme.font_code { font_size: 7.5 }
                }
            }
        }

        search_input := TextInput {
            width: Fill
            height: 26
            empty_text: "Filter files…"
            padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
            margin: Inset{left: 8 right: 8 top: 2 bottom: 6}
            draw_bg +: {
                color: theme.color_background
                color_hover: theme.color_background
                color_focus: theme.color_background
                border_color: theme.color_border
                border_color_focus: theme.color_primary
                border_radius: 4.0
                border_size: 1.0
            }
            draw_text +: {
                color: theme.color_foreground
                color_empty: theme.color_muted_foreground
                text_style +: {
                    font_size: 8.5
                }
            }
        }

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
                    text: "No workspace files"
                    draw_text +: {
                        color: theme.color_muted_foreground
                        text_style: theme.font_regular { font_size: 9.0 }
                    }
                }
            }

            Node := View {
                width: Fill
                height: 22
                flow: Right
                align: Align{y: 0.5}
                padding: Inset{left: 4 right: 4}

                node_btn := Button {
                    width: Fill
                    height: 22
                    padding: Inset{left: 6 right: 6}
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
pub struct FileTree {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    work_dir: Option<PathBuf>,
    #[rust]
    all_nodes: Vec<FileTreeNode>,
    #[rust]
    visible_nodes: Vec<FileTreeNode>,
    #[rust]
    open_dirs: HashSet<String>,
    #[rust]
    filter_query: String,
    #[rust]
    selected_path: Option<String>,
}

impl FileTree {
    pub fn set_work_dir(&mut self, cx: &mut Cx, work_dir: Option<PathBuf>) {
        if self.work_dir == work_dir {
            return;
        }
        self.work_dir = work_dir;
        self.reload_nodes();
        self.rebuild_visible_nodes();
        self.view.redraw(cx);
    }

    fn reload_nodes(&mut self) {
        self.all_nodes.clear();
        self.open_dirs.clear();

        let Some(root) = self.work_dir.as_ref() else {
            return;
        };
        if !root.is_dir() {
            return;
        }

        fn scan_dir_recursive(
            dir_path: &Path,
            root: &Path,
            depth: usize,
            parent_rel: Option<String>,
            all_nodes: &mut Vec<FileTreeNode>,
        ) {
            if depth > 4 || all_nodes.len() >= 1200 {
                return;
            }
            let Ok(read_dir) = fs::read_dir(dir_path) else {
                return;
            };
            let mut entries: Vec<_> = read_dir.filter_map(|res| res.ok()).collect();
            entries.sort_by(|a, b| {
                let a_is_dir = a.file_type().is_ok_and(|t| t.is_dir());
                let b_is_dir = b.file_type().is_ok_and(|t| t.is_dir());
                if a_is_dir != b_is_dir {
                    b_is_dir.cmp(&a_is_dir) // Directories first
                } else {
                    a.file_name().cmp(&b.file_name())
                }
            });

            for entry in entries {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || name == "vendor"
                {
                    continue;
                }
                let path = entry.path();
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                let rel_path = path
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| name.clone());

                all_nodes.push(FileTreeNode {
                    rel_path: rel_path.clone(),
                    name,
                    is_dir,
                    depth,
                    parent_rel_path: parent_rel.clone(),
                });

                if is_dir {
                    scan_dir_recursive(&path, root, depth + 1, Some(rel_path), all_nodes);
                }
            }
        }

        scan_dir_recursive(root, root, 0, None, &mut self.all_nodes);
    }

    fn rebuild_visible_nodes(&mut self) {
        self.visible_nodes.clear();
        let filter = self.filter_query.trim().to_lowercase();
        let filtering = !filter.is_empty();

        for node in &self.all_nodes {
            if filtering {
                if node.name.to_lowercase().contains(&filter)
                    || node.rel_path.to_lowercase().contains(&filter)
                {
                    self.visible_nodes.push(node.clone());
                }
            } else {
                let mut is_visible = true;
                let mut curr_parent = node.parent_rel_path.as_deref();
                while let Some(parent) = curr_parent {
                    if !self.open_dirs.contains(parent) {
                        is_visible = false;
                        break;
                    }
                    curr_parent = self
                        .all_nodes
                        .iter()
                        .find(|n| n.rel_path == parent)
                        .and_then(|n| n.parent_rel_path.as_deref());
                }
                if is_visible {
                    self.visible_nodes.push(node.clone());
                }
            }
        }
    }
}

impl Widget for FileTree {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let count_str = format!("{} items", self.visible_nodes.len());
        self.view.label(cx, ids!(count_lbl)).set_text(cx, &count_str);

        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.as_portal_list().borrow_mut() {
                if self.visible_nodes.is_empty() {
                    list.set_item_range(cx, 0, 0);
                    let item = list.item(cx, 0, id!(Empty));
                    item.draw_all_unscoped(cx);
                    continue;
                }
                list.set_item_range(cx, 0, self.visible_nodes.len());
                while let Some(row_index) = list.next_visible_item(cx) {
                    let Some(node) = self.visible_nodes.get(row_index) else {
                        continue;
                    };
                    let row = list.item(cx, row_index, id!(Node));

                    let filtering = !self.filter_query.trim().is_empty();
                    let display_text = if filtering {
                        let parent = Path::new(&node.rel_path)
                            .parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .filter(|p| !p.is_empty())
                            .unwrap_or_else(|| "root".to_string());
                        format!("   {} ({})", node.name, parent)
                    } else {
                        let prefix = if node.is_dir {
                            if self.open_dirs.contains(&node.rel_path) {
                                "v  "
                            } else {
                                ">  "
                            }
                        } else {
                            "   "
                        };
                        let indent = "  ".repeat(node.depth);
                        format!("{}{}{}", indent, prefix, node.name)
                    };

                    let btn = row.button(cx, ids!(node_btn));
                    if btn.text() != display_text {
                        btn.set_text(cx, &display_text);
                    }
                    row.draw_all_unscoped(cx);
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            let uid = self.widget_uid();

            if let Some(query) = self
                .view
                .text_input(cx, ids!(search_input))
                .changed(actions)
            {
                self.filter_query = query;
                self.rebuild_visible_nodes();
                self.view.redraw(cx);
            }

            let list = self.view.portal_list(cx, ids!(list));
            for (index, row) in list.items_with_actions(actions) {
                let Some(node) = self.visible_nodes.get(index).cloned() else {
                    continue;
                };
                if row.button(cx, ids!(node_btn)).clicked(actions) {
                    if node.is_dir {
                        if !self.open_dirs.insert(node.rel_path.clone()) {
                            self.open_dirs.remove(&node.rel_path);
                        }
                        self.rebuild_visible_nodes();
                        self.view.redraw(cx);
                        cx.widget_action(uid, FileTreeAction::FolderToggled(node.rel_path));
                    } else {
                        self.selected_path = Some(node.rel_path.clone());
                        cx.widget_action(uid, FileTreeAction::FileClicked(node.rel_path));
                    }
                }
            }
        }
    }
}
