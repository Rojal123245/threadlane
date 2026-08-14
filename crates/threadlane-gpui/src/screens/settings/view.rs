use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Selectable};

use crate::app::{actions::AppAction, controller};
use crate::state::AppState;

pub struct SettingsModalView {
    pub model: Entity<AppState>,
    pub openai_input: Entity<InputState>,
    pub opencode_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsModalView {
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

        let sub1 = cx.observe(&model, |_this, _model, cx| {
            cx.notify();
        });

        let model_clone1 = model.clone();
        let sub2 = cx.subscribe_in(
            &openai_input,
            window,
            move |_this, input_state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input_state.read(cx).value().to_string();
                    model_clone1.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::SaveOpenAiKey(key));
                    });
                }
            },
        );

        let model_clone2 = model.clone();
        let sub3 = cx.subscribe_in(
            &opencode_input,
            window,
            move |_this, input_state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input_state.read(cx).value().to_string();
                    model_clone2.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::SaveOpenCodeKey(key));
                    });
                }
            },
        );

        Self {
            model,
            openai_input,
            opencode_input,
            _subscriptions: vec![sub1, sub2, sub3],
        }
    }
}

impl Render for SettingsModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (selected_model, auth_status) = {
            let state = self.model.read(cx);
            (state.selected_model.clone(), state.auth_status_msg.clone())
        };
        let theme = cx.theme().colors;
        let active_theme = crate::theme::active_theme_name(cx);
        let theme_options = crate::theme::available_themes(cx);
        let model = self.model.clone();
        let openai_input_state = self.openai_input.clone();
        let opencode_input_state = self.opencode_input.clone();

        let model_options = vec![
            ("gpt-4o", "OpenAI (GPT-4o)"),
            ("gpt-4o-mini", "OpenAI (GPT-4o Mini)"),
            (
                "antigravity/gemini-3.6-flash",
                "Google Antigravity (Gemini 3.6 Flash)",
            ),
            (
                "opencode-go/claude-3-5-sonnet",
                "Opencode (Claude 3.5 Sonnet)",
            ),
        ];

        div()
            .absolute()
            .inset_0()
            .bg(theme.background.opacity(0.8))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(520.0))
                    .p_6()
                    .rounded_xl()
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_5()
                    // Modal Header
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme.border)
                            .pb_3()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.foreground)
                                    .child("Provider & Model Settings"),
                            )
                            .child(
                                Button::new("settings-close-btn")
                                    .label("✕ Close")
                                    .ghost()
                                    .on_click({
                                        let model = model.clone();
                                        move |_event, _window, cx| {
                                            model.update(cx, |state, _cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::ToggleSettings,
                                                );
                                            });
                                        }
                                    }),
                            ),
                    )
                    // Status Notification if any
                    .children(if let Some(status) = auth_status {
                        Some(
                            div()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(theme.success)
                                .border_1()
                                .border_color(theme.success)
                                .text_xs()
                                .text_color(theme.success_foreground)
                                .child(status),
                        )
                    } else {
                        None
                    })
                    // Appearance Section
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child("Appearance"),
                            )
                            .child(div().flex().flex_col().gap_1p5().children(
                                theme_options.into_iter().map(|(name, _mode)| {
                                    let selected = active_theme == name;
                                    let button_name = name.clone();
                                    Button::new(SharedString::from(format!(
                                        "theme-{}",
                                        name.to_lowercase().replace(' ', "-")
                                    )))
                                    .label(name)
                                    .ghost()
                                    .selected(selected)
                                    .on_click(
                                        move |_event, _window, cx| {
                                            crate::theme::apply_theme(&button_name, cx);
                                        },
                                    )
                                }),
                            )),
                    )
                    // Model Selection Section
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child("Active AI Model"),
                            )
                            .child(div().flex().flex_col().gap_1p5().children(
                                model_options.into_iter().map(|(id, label)| {
                                    let is_selected = selected_model == id;
                                    let model = model.clone();
                                    let id_str = id.to_string();

                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .px_3()
                                        .py_2()
                                        .rounded_lg()
                                        .bg(if is_selected {
                                            theme.primary
                                        } else {
                                            theme.secondary
                                        })
                                        .border_1()
                                        .border_color(if is_selected {
                                            theme.ring
                                        } else {
                                            theme.border
                                        })
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_event, _window, cx| {
                                                let id_str = id_str.clone();
                                                model.update(cx, |state, _cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::SelectModel(id_str),
                                                    );
                                                });
                                            },
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(if is_selected {
                                                    theme.primary_foreground
                                                } else {
                                                    theme.secondary_foreground
                                                })
                                                .child(label),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(if is_selected {
                                                    theme.primary_foreground
                                                } else {
                                                    theme.muted_foreground
                                                })
                                                .child(if is_selected {
                                                    "Active"
                                                } else {
                                                    "Select"
                                                }),
                                        )
                                }),
                            )),
                    )
                    // OpenAI API Key Section
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child("OpenAI API Key (sk-...)"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div().flex_1().child(
                                            Input::new(&self.openai_input)
                                                .appearance(false)
                                                .bordered(false),
                                        ),
                                    )
                                    .child(
                                        Button::new("save-openai-key-btn")
                                            .label("Save Key")
                                            .primary()
                                            .on_click({
                                                let model = model.clone();
                                                let openai_input = openai_input_state.clone();
                                                move |_event, _window, cx| {
                                                    let key =
                                                        openai_input.read(cx).value().to_string();
                                                    model.update(cx, |state, _cx| {
                                                        controller::dispatch(
                                                            state,
                                                            AppAction::SaveOpenAiKey(key),
                                                        );
                                                    });
                                                }
                                            }),
                                    ),
                            ),
                    )
                    // Opencode API Key Section
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child("Opencode API Key"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div().flex_1().child(
                                            Input::new(&self.opencode_input)
                                                .appearance(false)
                                                .bordered(false),
                                        ),
                                    )
                                    .child(
                                        Button::new("save-opencode-key-btn")
                                            .label("Save Key")
                                            .ghost()
                                            .on_click({
                                                let model = model.clone();
                                                let opencode_input = opencode_input_state.clone();
                                                move |_event, _window, cx| {
                                                    let key =
                                                        opencode_input.read(cx).value().to_string();
                                                    model.update(cx, |state, _cx| {
                                                        controller::dispatch(
                                                            state,
                                                            AppAction::SaveOpenCodeKey(key),
                                                        );
                                                    });
                                                }
                                            }),
                                    ),
                            ),
                    ),
            )
    }
}
