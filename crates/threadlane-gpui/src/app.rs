use gpui::*;

use crate::chat_list::ChatListView;
use crate::sidebar::SidebarView;
use crate::state::AppState;

pub struct WorkspaceView {
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
}

impl WorkspaceView {
    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| AppState::load());
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model, window, cx));

        cx.new(|_cx| Self { sidebar, chat_list })
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .h_full()
            .bg(rgb(0x09090b))
            .child(self.sidebar.clone())
            .child(self.chat_list.clone())
    }
}
