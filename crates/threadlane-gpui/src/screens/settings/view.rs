use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, IconName, Selectable};

use crate::app::{actions::AppAction, controller};
use crate::state::AppState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    Appearance,
    Providers,
}

pub struct SettingsView {
    model: Entity<AppState>,
    openai_input: Entity<InputState>,
    opencode_input: Entity<InputState>,
    page: SettingsPage,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (openai_key, opencode_key) = {
            let state = model.read(cx);
            (state.openai_key.clone(), state.opencode_key.clone())
        };

        let openai_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("sk-proj-...")
                .default_value(&openai_key)
                .masked(true)
        });
        let opencode_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("opencode-key-...")
                .default_value(&opencode_key)
                .masked(true)
        });

        let observe_model = cx.observe(&model, |_this, _model, cx| cx.notify());
        let openai_model = model.clone();
        let save_openai = cx.subscribe_in(
            &openai_input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input.read(cx).value().to_string();
                    openai_model.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::SaveOpenAiKey(key));
                    });
                }
            },
        );
        let opencode_model = model.clone();
        let save_opencode = cx.subscribe_in(
            &opencode_input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input.read(cx).value().to_string();
                    opencode_model.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::SaveOpenCodeKey(key));
                    });
                }
            },
        );

        Self {
            model,
            openai_input,
            opencode_input,
            page: SettingsPage::default(),
            _subscriptions: vec![observe_model, save_openai, save_opencode],
        }
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();

        div()
            .w(px(260.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.sidebar_border)
            .bg(theme.sidebar)
            .child(div().h(px(48.0)).flex_none())
            .child(
                div().px_3().child(
                    Button::new("settings-back")
                        .icon(IconName::ArrowLeft)
                        .label("Back")
                        .ghost()
                        .w_full()
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, _cx| {
                                controller::dispatch(state, AppAction::CloseSettings);
                            });
                        }),
                ),
            )
            .child(
                div()
                    .px_3()
                    .pt_4()
                    .pb_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("SETTINGS"),
            )
            .child(
                div()
                    .px_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Button::new("settings-appearance")
                            .icon(IconName::Palette)
                            .label("Appearance")
                            .ghost()
                            .selected(self.page == SettingsPage::Appearance)
                            .w_full()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Appearance;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-providers")
                            .icon(IconName::Bot)
                            .label("Providers")
                            .ghost()
                            .selected(self.page == SettingsPage::Providers)
                            .w_full()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Providers;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let active_theme = crate::theme::active_theme_name(cx);
        let themes = crate::theme::available_themes(cx);

        div()
            .mt_5()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .p_4()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Theme"),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Choose a bundled theme or one installed in ~/.threadlane/themes."),
            )
            .child(
                div()
                    .mt_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(themes.into_iter().map(|(name, _mode)| {
                        let selected = name == active_theme;
                        let apply_name = name.clone();
                        Button::new(SharedString::from(format!(
                            "settings-theme-{}",
                            name.to_lowercase().replace(' ', "-")
                        )))
                        .label(name)
                        .selected(selected)
                        .ghost()
                        .w_full()
                        .on_click(move |_event, _window, cx| {
                            crate::theme::apply_theme(&apply_name, cx);
                        })
                    })),
            )
            .into_any_element()
    }

    fn render_key_row(
        &self,
        label: &'static str,
        input: &Entity<InputState>,
        button_id: &'static str,
        action: fn(String) -> AppAction,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let input = input.clone();

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(label),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&input).mask_toggle()))
                    .child(Button::new(button_id).label("Save").primary().on_click(
                        move |_event, _window, cx| {
                            let value = input.read(cx).value().to_string();
                            model.update(cx, |state, _cx| {
                                controller::dispatch(state, action(value));
                            });
                        },
                    )),
            )
    }

    fn render_providers(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let (selected_model, status) = {
            let state = self.model.read(cx);
            (state.selected_model.clone(), state.auth_status_msg.clone())
        };
        let model = self.model.clone();
        let model_options = [
            ("gpt-4o", "OpenAI · GPT-4o"),
            ("gpt-4o-mini", "OpenAI · GPT-4o Mini"),
            (
                "antigravity/gemini-3.6-flash",
                "Google Antigravity · Gemini 3.6 Flash",
            ),
            (
                "opencode-go/claude-3-5-sonnet",
                "OpenCode · Claude 3.5 Sonnet",
            ),
        ];

        div()
            .mt_5()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .px_4()
            .children(status.map(|status| {
                div()
                    .mt_4()
                    .rounded_md()
                    .bg(theme.success)
                    .p_3()
                    .text_xs()
                    .text_color(theme.success_foreground)
                    .child(status)
            }))
            .child(
                div()
                    .py_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child("Default model"),
                    )
                    .child(div().mt_3().flex().flex_col().gap_2().children(
                        model_options.into_iter().map(|(id, label)| {
                            let selected = selected_model == id;
                            let model = model.clone();
                            let id = id.to_string();
                            Button::new(SharedString::from(format!("settings-model-{id}")))
                                .label(label)
                                .selected(selected)
                                .ghost()
                                .w_full()
                                .on_click(move |_event, _window, cx| {
                                    model.update(cx, |state, _cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::SelectModel(id.clone()),
                                        );
                                    });
                                })
                        }),
                    )),
            )
            .child(self.render_key_row(
                "OpenAI API key",
                &self.openai_input,
                "save-openai-key",
                AppAction::SaveOpenAiKey,
                cx,
            ))
            .child(self.render_key_row(
                "OpenCode API key",
                &self.opencode_input,
                "save-opencode-key",
                AppAction::SaveOpenCodeKey,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (title, description, content) = match self.page {
            SettingsPage::Appearance => (
                "Appearance",
                "Customize how Threadlane looks.",
                self.render_appearance(cx),
            ),
            SettingsPage::Providers => (
                "Providers",
                "Configure models and provider credentials.",
                self.render_providers(cx),
            ),
        };

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .bg(theme.background)
            .child(self.render_navigation(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_y_scrollbar()
                    .px_8()
                    .pb_8()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(760.0))
                            .mx_auto()
                            .child(div().h(px(48.0)).flex_none())
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(description),
                            )
                            .child(content),
                    ),
            )
    }
}
