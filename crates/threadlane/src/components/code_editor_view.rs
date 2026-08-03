//! Read/write code editor backed by `makepad-code-editor`.
//!
//! Modeled on Makepad Studio's `DesktopCodeEditor`: the upstream `CodeEditor`
//! is not an ordinary auto-drawn child, because drawing and event handling both
//! need a `CodeSession` threaded through. Studio keeps sessions in shared app
//! data keyed by dock tab; Threadlane shows one file at a time, so this widget
//! owns its session outright and no scope plumbing is needed.
//!
//! Chrome (file name, close button) lives in the app DSL around this widget,
//! matching how `file_tree_wrap` wraps the file tree.

use makepad_code_editor::decoration::DecorationSet;
use makepad_code_editor::{CodeDocument, CodeEditor, CodeSession};
use makepad_widgets::*;
use std::path::{Path, PathBuf};

/// Files above this size are refused rather than loaded, so a stray click on a
/// large build artifact cannot stall the UI while it tokenizes.
const MAX_EDITABLE_BYTES: u64 = 2 * 1024 * 1024;

/// Reads a file the editor is willing to open.
///
/// Split out from [`CodeEditorView::open_file`] so the size and encoding rules
/// are testable without a `Cx`.
fn load_editable_text(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if metadata.is_dir() {
        return Err(format!("{} is a directory.", path.display()));
    }
    if metadata.len() > MAX_EDITABLE_BYTES {
        return Err(format!(
            "{} is {:.1} MB; files over {} MB are not opened in the editor.",
            path.display(),
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_EDITABLE_BYTES / (1024 * 1024)
        ));
    }
    // Binary files would render as replacement characters and be corrupted on
    // save, so they are refused rather than shown.
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{} is not a UTF-8 text file.", path.display()))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum CodeEditorViewAction {
    /// The buffer changed relative to what is on disk.
    Modified,
    #[default]
    None,
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.components.CodeEditorViewBase = #(CodeEditorView::register_widget(vm))

    // Sizing lives on the inner `CodeEditor`, whose own default is already
    // Fill/Fill. This wrapper has no `#[walk]` of its own; it delegates `walk()`
    // to the editor, so width/height set here would not resolve.
    mod.components.CodeEditorView = set_type_default() do mod.components.CodeEditorViewBase {
        editor := CodeEditor {}
    }
}

#[derive(Script, ScriptHook, WidgetRef, WidgetSet, WidgetRegister)]
pub struct CodeEditorView {
    #[uid]
    uid: WidgetUid,
    #[live]
    editor: CodeEditor,
    #[rust]
    session: Option<CodeSession>,
    #[rust]
    path: Option<PathBuf>,
    #[rust]
    modified: bool,
}

impl CodeEditorView {
    /// Loads `path` into a fresh session. Returns an error message suitable for
    /// display when the file cannot be opened.
    pub fn open_file(&mut self, cx: &mut Cx, path: &Path) -> Result<(), String> {
        let text = load_editable_text(path)?;
        self.session = Some(CodeSession::new(CodeDocument::new(
            text.as_str().into(),
            DecorationSet::new(),
        )));
        self.path = Some(path.to_path_buf());
        self.modified = false;
        self.editor.set_key_focus(cx);
        self.redraw(cx);
        Ok(())
    }

    pub fn close(&mut self, cx: &mut Cx) {
        self.session = None;
        self.path = None;
        self.modified = false;
        self.redraw(cx);
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Writes the buffer back to the file it was opened from.
    pub fn save(&mut self) -> Result<(), String> {
        let (Some(session), Some(path)) = (self.session.as_ref(), self.path.as_ref()) else {
            return Err("No file is open in the editor.".to_string());
        };
        let text = session.document().as_text().to_string();
        std::fs::write(path, text)
            .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
        self.modified = false;
        Ok(())
    }
}

impl WidgetNode for CodeEditorView {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }

    fn walk(&mut self, cx: &mut Cx) -> Walk {
        self.editor.walk(cx)
    }

    fn area(&self) -> Area {
        self.editor.area()
    }

    fn redraw(&mut self, cx: &mut Cx) {
        self.editor.redraw(cx)
    }

    fn find_widgets_from_point(&self, cx: &Cx, point: DVec2, found: &mut dyn FnMut(&WidgetRef)) {
        self.editor.find_widgets_from_point(cx, point, found)
    }

    fn visible(&self) -> bool {
        self.editor.visible()
    }

    fn set_visible(&mut self, cx: &mut Cx, visible: bool) {
        self.editor.set_visible(cx, visible)
    }
}

impl Widget for CodeEditorView {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        match self.session.as_mut() {
            Some(session) => self.editor.draw_walk_editor(cx, session, walk),
            None => self.editor.draw_empty_editor(cx, walk),
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let actions = self
            .editor
            .handle_event(cx, event, &mut Scope::empty(), session);
        if actions.is_empty() {
            return;
        }
        // Any editor action means the buffer moved away from what is on disk.
        // Tracking it here keeps the "unsaved" marker owned by the widget that
        // knows about the edit rather than by the app shell.
        if !self.modified {
            self.modified = true;
            cx.widget_action(self.uid, CodeEditorViewAction::Modified);
        }
    }
}

impl CodeEditorViewRef {
    pub fn open_file(&self, cx: &mut Cx, path: &Path) -> Result<(), String> {
        let Some(mut inner) = self.borrow_mut() else {
            return Err("Editor is unavailable.".to_string());
        };
        inner.open_file(cx, path)
    }

    pub fn close(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.close(cx);
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(mut inner) = self.borrow_mut() else {
            return Err("Editor is unavailable.".to_string());
        };
        inner.save()
    }

    pub fn is_open(&self) -> bool {
        self.borrow().is_some_and(|inner| inner.is_open())
    }

    pub fn is_modified(&self) -> bool {
        self.borrow().is_some_and(|inner| inner.is_modified())
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.borrow()
            .and_then(|inner| inner.path().map(Path::to_path_buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_code_editor::text::Text;

    #[test]
    fn plain_text_files_load() {
        let dir = std::env::temp_dir().join("threadlane_editor_text");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("main.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        assert_eq!(load_editable_text(&path).unwrap(), "fn main() {}\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_files_report_the_path() {
        let error = load_editable_text(Path::new("/nonexistent/threadlane/file.rs")).unwrap_err();
        assert!(error.contains("Could not read"), "got: {error}");
        assert!(error.contains("file.rs"), "got: {error}");
    }

    #[test]
    fn directories_are_refused() {
        let dir = std::env::temp_dir().join("threadlane_editor_dir");
        std::fs::create_dir_all(&dir).unwrap();
        let error = load_editable_text(&dir).unwrap_err();
        assert!(error.contains("is a directory"), "got: {error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn binary_files_are_refused_rather_than_mangled() {
        let dir = std::env::temp_dir().join("threadlane_editor_binary");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob.bin");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).unwrap();

        let error = load_editable_text(&path).unwrap_err();
        assert!(error.contains("not a UTF-8 text file"), "got: {error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_files_are_refused_with_their_size() {
        let dir = std::env::temp_dir().join("threadlane_editor_large");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("huge.log");
        std::fs::write(&path, vec![b'x'; (MAX_EDITABLE_BYTES + 1) as usize]).unwrap();

        let error = load_editable_text(&path).unwrap_err();
        assert!(
            error.contains("are not opened in the editor"),
            "got: {error}"
        );
        assert!(error.contains("MB"), "got: {error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn document_text_round_trips_through_a_save() {
        // `save` writes `Text`'s Display form, so a load/store cycle has to be
        // byte-identical or editing a file would rewrite its line endings.
        for source in ["fn main() {}\n", "a\nb\nc", "", "trailing\n\n"] {
            let text: Text = source.into();
            assert_eq!(text.to_string(), source, "round trip failed for {source:?}");
        }
    }
}
