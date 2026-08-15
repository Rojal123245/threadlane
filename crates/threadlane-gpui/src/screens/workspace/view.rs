use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Sizable};

use crate::screens::chat::ChatListView;
use crate::screens::right_panel::RightPanelView;
use crate::screens::settings::SettingsView;
use crate::screens::sidebar::SidebarView;
use crate::state::{AppState, WorkspacePage};

pub struct WorkspaceView {
    model: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
    settings: Entity<SettingsView>,
    right_panel: Entity<RightPanelView>,
    sidebar_collapsed: bool,
    right_panel_visible: bool,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| AppState::load());
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model.clone(), window, cx));
        let settings = cx.new(|cx| SettingsView::new(model.clone(), window, cx));
        let right_panel = cx.new(|cx| RightPanelView::new(model.clone(), window, cx));

        let model_clone = model.clone();
        cx.new(|cx| {
            let sub = cx.observe(&model_clone, |_this, _model, cx| {
                cx.notify();
            });

            Self {
                model,
                sidebar,
                chat_list,
                settings,
                right_panel,
                sidebar_collapsed: false,
                right_panel_visible: false,
                _subscriptions: vec![sub],
            }
        })
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace_page = self.model.read(cx).workspace_page;
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
            .children(
                (workspace_page == WorkspacePage::Chat && !self.sidebar_collapsed)
                    .then(|| self.sidebar.clone()),
            )
            .child(match workspace_page {
                WorkspacePage::Chat => div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(360.0))
                            .h_full()
                            .child(self.chat_list.clone()),
                    )
                    .children(self.right_panel_visible.then(|| {
                        div()
                            .flex_1()
                            .min_w(px(360.0))
                            .h_full()
                            .child(self.right_panel.clone())
                    }))
                    .into_any_element(),
                WorkspacePage::Settings => self.settings.clone().into_any_element(),
            })
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("right-panel-toggle")
                    .icon(IconName::PanelRight)
                    .tooltip(if self.right_panel_visible {
                        "Hide right panel"
                    } else {
                        "Show right panel"
                    })
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(12.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.right_panel_visible = !this.right_panel_visible;
                        cx.notify();
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
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
                    }))
            }))
    }
}
