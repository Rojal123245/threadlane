use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Sizable};

use crate::screens::chat::ChatListView;
use crate::screens::settings::SettingsModalView;
use crate::screens::sidebar::SidebarView;
use crate::state::AppState;

pub struct WorkspaceView {
    model: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
    settings_modal: Entity<SettingsModalView>,
    sidebar_collapsed: bool,
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
                sidebar_collapsed: false,
                _subscriptions: vec![sub],
            }
        })
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_settings_open = self.model.read(cx).is_settings_open;
        let theme = cx.theme().colors;
        let sidebar_tooltip = if self.sidebar_collapsed {
            "Show sidebar"
        } else {
            "Collapse sidebar"
        };

        div()
            .relative()
            .flex()
            .w_full()
            .h_full()
            .bg(theme.background)
            .children((!self.sidebar_collapsed).then(|| self.sidebar.clone()))
            .child(self.chat_list.clone())
            .child(
                Button::new("sidebar-collapse-toggle")
                    .icon(IconName::PanelLeft)
                    .tooltip(sidebar_tooltip)
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .left(px(76.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        let inset = if this.sidebar_collapsed {
                            px(110.0)
                        } else {
                            px(14.0)
                        };
                        this.chat_list.update(cx, |chat, cx| {
                            chat.header_left_padding = inset;
                            cx.notify();
                        });
                        cx.notify();
                    })),
            )
            .children(if is_settings_open {
                Some(self.settings_modal.clone())
            } else {
                None
            })
    }
}
