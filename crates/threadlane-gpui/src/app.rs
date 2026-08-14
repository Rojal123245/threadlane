use gpui::*;

use crate::chat_list::ChatListView;
use crate::settings_modal::SettingsModalView;
use crate::sidebar::SidebarView;
use crate::state::AppState;

pub struct WorkspaceView {
    model: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
    settings_modal: Entity<SettingsModalView>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| AppState::load());
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model.clone(), window, cx));
        let settings_modal = cx.new(|cx| SettingsModalView::new(model.clone(), window, cx));

        let model_clone = model.clone();
        cx.new(|cx| {
            let sub = cx.observe(&model_clone, |_this, _model, cx| {
                cx.notify();
            });

            Self {
                model,
                sidebar,
                chat_list,
                settings_modal,
                _subscriptions: vec![sub],
            }
        })
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_settings_open = self.model.read(cx).is_settings_open;

        div()
            .relative()
            .flex()
            .w_full()
            .h_full()
            .bg(rgb(0x09090b))
            .child(self.sidebar.clone())
            .child(self.chat_list.clone())
            .children(if is_settings_open {
                Some(self.settings_modal.clone())
            } else {
                None
            })
    }
}
