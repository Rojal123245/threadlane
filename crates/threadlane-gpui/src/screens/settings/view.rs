use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::alert::{Alert, AlertVariant};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::switch::Switch;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};

use crate::app::{actions::AppAction, controller};
use crate::services::provider_auth::{self, ProviderAuthEvent};
use crate::services::settings::{self, SettingsEvent};
use crate::state::AppState;
use threadlane_coding_agent::{
    AcpAgentRecord, AcpScope, ExtensionRecord, ExtensionScope, SkillMetadata,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    Appearance,
    Providers,
    Extensions,
    Skills,
    AcpAgents,
}

pub struct SettingsView {
    model: Entity<AppState>,
    openai_input: Entity<InputState>,
    opencode_input: Entity<InputState>,
    acp_name_input: Entity<InputState>,
    acp_command_input: Entity<InputState>,
    page: SettingsPage,
    install_globally: bool,
    extension_rows: Vec<ExtensionRecord>,
    skill_rows: Vec<SkillMetadata>,
    acp_rows: Vec<AcpAgentRecord>,
    capability_status: Option<String>,
    auth_tx: Sender<ProviderAuthEvent>,
    settings_tx: Sender<SettingsEvent>,
    auth_message: Option<String>,
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
        let acp_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Claude Code"));
        let acp_command_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("npx -y @zed-industries/claude-code-acp")
        });

        let (auth_tx, auth_rx) = mpsc::channel();
        let auth_model = model.clone();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let events = auth_rx.try_iter().collect::<Vec<_>>();
            if events.is_empty() {
                continue;
            }
            let _ = this.update(cx, |this, cx| {
                for event in events {
                    let credentials_changed = matches!(event, ProviderAuthEvent::Connected(_));
                    this.auth_message = Some(match event {
                        ProviderAuthEvent::Status(message)
                        | ProviderAuthEvent::Connected(message)
                        | ProviderAuthEvent::Error(message) => message,
                    });
                    if credentials_changed {
                        auth_model.update(cx, |state, cx| {
                            state.reconcile_selected_model();
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();

        let (settings_tx, settings_rx) = mpsc::channel();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let events = settings_rx.try_iter().collect::<Vec<_>>();
            if events.is_empty() {
                continue;
            }
            let _ = this.update(cx, |this, cx| {
                for event in events {
                    match event {
                        SettingsEvent::AcpRefreshed(records) => this.acp_rows = records,
                    }
                }
                cx.notify();
            });
        })
        .detach();

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
            acp_name_input,
            acp_command_input,
            page: SettingsPage::default(),
            install_globally: false,
            extension_rows: Vec::new(),
            skill_rows: Vec::new(),
            acp_rows: Vec::new(),
            capability_status: None,
            auth_tx,
            settings_tx,
            auth_message: None,
            _subscriptions: vec![observe_model, save_openai, save_opencode],
        }
    }

    fn active_project(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.model.read(cx).active_work_dir.clone()
    }

    fn refresh_extensions(&mut self, cx: &mut Context<Self>) {
        self.extension_rows = settings::discover_extensions(self.active_project(cx));
    }

    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        let project = self.active_project(cx);
        self.skill_rows = settings::discover_skills(project.as_deref());
    }

    fn refresh_acp(&mut self, cx: &mut Context<Self>) {
        let project = self.active_project(cx);
        self.acp_rows = settings::configured_acp_agents(project.clone());
        self.model.update(cx, |state, cx| {
            state.reconcile_selected_model();
            cx.notify();
        });
        if let Err(error) = settings::probe_acp_agents(project, self.settings_tx.clone()) {
            self.capability_status = Some(error);
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
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_start()
                                .gap_2()
                                .child(IconName::ArrowLeft)
                                .child("Back"),
                        )
                        .ghost()
                        .w_full()
                        .justify_start()
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
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Palette)
                                    .child("Appearance"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Appearance)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Appearance;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-providers")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Bot)
                                    .child("Providers"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Providers)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Providers;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-extensions")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(Icon::default().path("icons/hard-drive.svg"))
                                    .child("Extensions"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Extensions)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Extensions;
                                this.capability_status = None;
                                this.refresh_extensions(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-skills")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(Icon::default().path("icons/book-open.svg"))
                                    .child("Skills"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Skills)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Skills;
                                this.capability_status = None;
                                this.refresh_skills(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-acp")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(Icon::default().path("icons/providers/acp.svg"))
                                    .child("ACP Agents"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::AcpAgents)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::AcpAgents;
                                this.capability_status = None;
                                this.refresh_acp(cx);
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let active_theme = crate::theme::active_theme_name(cx);
        let themes = crate::theme::available_themes(cx);
        let menu_themes = themes.clone();
        let selected_theme = active_theme.clone();
        let theme_picker = Button::new("settings-theme-picker")
            .label(active_theme)
            .dropdown_caret(true)
            .outline()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for (name, _mode) in menu_themes.clone() {
                    let selected = name == selected_theme;
                    let apply_name = name.clone();
                    menu = menu.item(PopupMenuItem::new(name).checked(selected).on_click(
                        move |_event, _window, cx| {
                            crate::theme::apply_theme(&apply_name, cx);
                        },
                    ));
                }
                menu
            });

        div()
            .mt_5()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .p_4()
            .flex()
            .items_center()
            .gap_6()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
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
                            .child(
                                "Choose a bundled theme or one installed in ~/.threadlane/themes.",
                            ),
                    ),
            )
            .child(theme_picker)
            .into_any_element()
    }

    fn render_provider_connection(
        &self,
        title: &'static str,
        description: &'static str,
        connected: bool,
        antigravity: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let model = self.model.clone();
        let button_label = if connected {
            "Disconnect"
        } else if antigravity {
            "Sign in with Google"
        } else {
            "Sign in with ChatGPT"
        };

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .w(px(36.0))
                    .h(px(36.0))
                    .flex_none()
                    .rounded_lg()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(if connected {
                        theme.success
                    } else {
                        theme.muted_foreground
                    })
                    .child(IconName::Bot),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                Tag::new()
                                    .child(if connected { "Connected" } else { "Not connected" })
                                    .with_variant(if connected {
                                        TagVariant::Success
                                    } else {
                                        TagVariant::Secondary
                                    })
                                    .small(),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(description),
                    ),
            )
            .child(
                Button::new(if antigravity {
                    "antigravity-auth-button"
                } else {
                    "chatgpt-auth-button"
                })
                .label(button_label)
                .when(!connected, |button| button.primary())
                .when(connected, |button| button.ghost())
                .on_click(move |_event, _window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if connected {
                            let result = if antigravity {
                                threadlane_provider::antigravity_auth::clear_antigravity_credentials()
                            } else {
                                threadlane_auth::openai_auth::remove_credentials()
                            };
                            let disconnected = result.is_ok();
                            this.auth_message = Some(match result {
                                Ok(()) if antigravity => {
                                    "Disconnected Google Antigravity.".to_string()
                                }
                                Ok(()) => "Disconnected ChatGPT.".to_string(),
                                Err(error) => format!("Failed to disconnect: {error}"),
                            });
                            if disconnected {
                                model.update(cx, |state, cx| {
                                    state.reconcile_selected_model();
                                    cx.notify();
                                });
                            }
                        } else {
                            let result = if antigravity {
                                provider_auth::start_antigravity_login(this.auth_tx.clone())
                            } else {
                                provider_auth::start_chatgpt_login(this.auth_tx.clone())
                            };
                            this.auth_message = Some(match result {
                                Ok(()) if antigravity => {
                                    "Opening Google Antigravity sign-in...".to_string()
                                }
                                Ok(()) => "Starting ChatGPT sign-in...".to_string(),
                                Err(error) => error,
                            });
                        }
                        cx.notify();
                    });
                }),
            )
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
        let (selected_model, state_status) = {
            let state = self.model.read(cx);
            (state.selected_model.clone(), state.auth_status_msg.clone())
        };
        let status = self.auth_message.clone().or(state_status);
        let chatgpt_connected =
            threadlane_auth::openai_auth::load_credentials().is_some_and(|credentials| {
                threadlane_auth::openai_auth::is_own_source(&credentials.source)
            });
        let antigravity_connected =
            threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some();
        let model = self.model.clone();
        let project_root = self.active_project(cx);
        let model_options =
            crate::model_catalog::available_models_for_project(project_root.as_deref());
        let has_models = !model_options.is_empty();
        let selected_option = crate::model_catalog::available_option_for_project(
            &selected_model,
            project_root.as_deref(),
        );
        let selected_model_label = selected_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "Connect a provider".to_string());
        let picker_model = model.clone();
        let picker_selected_model = selected_model.clone();
        let model_picker = Button::new("settings-default-model-picker")
            .label(selected_model_label)
            .dropdown_caret(true)
            .outline()
            .disabled(!has_models);
        let model_picker = if let Some(option) = selected_option.as_ref() {
            model_picker.icon(Icon::default().path(option.provider.icon_path()))
        } else {
            model_picker
        };
        let task_model_options = model_options.clone();
        let model_picker = model_picker.dropdown_menu(move |menu, _window, _cx| {
            let mut menu = menu;
            for option in task_model_options.iter().cloned() {
                let selected = option.id == picker_selected_model;
                let id = option.id.to_string();
                let model = picker_model.clone();
                menu = menu.item(
                    PopupMenuItem::new(option.label)
                        .icon(Icon::default().path(option.provider.icon_path()))
                        .checked(selected)
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, _cx| {
                                controller::dispatch(state, AppAction::SelectModel(id.clone()));
                            });
                        }),
                );
            }
            menu
        });

        let (plan_model_id, advisor_model_id, advisor_enabled) = {
            let state = self.model.read(cx);
            (
                state.model_roles.plan.clone().unwrap_or_else(|| selected_model.clone()),
                state.model_roles.advisor.clone().unwrap_or_else(|| selected_model.clone()),
                state.model_roles.advisor_enabled,
            )
        };

        let plan_option = crate::model_catalog::available_option_for_project(
            &plan_model_id,
            project_root.as_deref(),
        );
        let plan_model_label = plan_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "Default (same as Task)".to_string());
        let plan_picker_model = model.clone();
        let plan_picker_selected = plan_model_id.clone();
        let plan_model_options = model_options.clone();
        let plan_picker = Button::new("settings-plan-model-picker")
            .label(plan_model_label)
            .dropdown_caret(true)
            .outline()
            .disabled(!has_models);
        let plan_picker = if let Some(option) = plan_option.as_ref() {
            plan_picker.icon(Icon::default().path(option.provider.icon_path()))
        } else {
            plan_picker
        };
        let plan_picker = plan_picker.dropdown_menu(move |menu, _window, _cx| {
            let mut menu = menu;
            for option in plan_model_options.iter().cloned() {
                let selected = option.id == plan_picker_selected;
                let id = option.id.to_string();
                let model = plan_picker_model.clone();
                menu = menu.item(
                    PopupMenuItem::new(option.label)
                        .icon(Icon::default().path(option.provider.icon_path()))
                        .checked(selected)
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                let mut roles = state.model_roles.clone();
                                roles.plan = Some(id.clone());
                                state.update_model_roles(roles);
                                cx.notify();
                            });
                        }),
                );
            }
            menu
        });

        let advisor_option = crate::model_catalog::available_option_for_project(
            &advisor_model_id,
            project_root.as_deref(),
        );
        let advisor_model_label = advisor_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "Default (same as Task)".to_string());
        let advisor_picker_model = model.clone();
        let advisor_picker_selected = advisor_model_id.clone();
        let advisor_model_options = model_options.clone();
        let advisor_picker = Button::new("settings-advisor-model-picker")
            .label(advisor_model_label)
            .dropdown_caret(true)
            .outline()
            .disabled(!has_models);
        let advisor_picker = if let Some(option) = advisor_option.as_ref() {
            advisor_picker.icon(Icon::default().path(option.provider.icon_path()))
        } else {
            advisor_picker
        };
        let advisor_picker = advisor_picker.dropdown_menu(move |menu, _window, _cx| {
            let mut menu = menu;
            for option in advisor_model_options.iter().cloned() {
                let selected = option.id == advisor_picker_selected;
                let id = option.id.to_string();
                let model = advisor_picker_model.clone();
                menu = menu.item(
                    PopupMenuItem::new(option.label)
                        .icon(Icon::default().path(option.provider.icon_path()))
                        .checked(selected)
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                let mut roles = state.model_roles.clone();
                                roles.advisor = Some(id.clone());
                                state.update_model_roles(roles);
                                cx.notify();
                            });
                        }),
                );
            }
            menu
        });


        let advisor_toggle_model = model.clone();

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
                    .bg(theme.muted)
                    .p_3()
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(TextView::markdown("provider-auth-status", status).selectable(true))
            }))
            .child(self.render_provider_connection(
                "OpenAI / ChatGPT",
                "GPT and Codex models via ChatGPT device login or an API key.",
                chatgpt_connected,
                false,
                cx,
            ))
            .child(self.render_provider_connection(
                "Google Antigravity",
                "Gemini and other models via Google OAuth PKCE.",
                antigravity_connected,
                true,
                cx,
            ))
            .child(
                div()
                    .py_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap_6()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Task Model (Execution)"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Main coding model used for executing tools and code modifications."),
                            ),
                    )
                    .child(model_picker),
            )
            .child(
                div()
                    .py_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap_6()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Plan Model (Architecture)"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("High-reasoning model used for /plan breakdown and architecture decomposition."),
                            ),
                    )
                    .child(plan_picker),
            )
            .child(
                div()
                    .py_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap_6()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Advisor Model (Reviewer)"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Secondary model paired to review turns and catch blockers."),
                            ),
                    )
                    .child(advisor_picker),
            )
            .child(
                div()
                    .py_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap_6()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Advisor Turn-Watcher"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Runs the advisor model on every turn to inject inline asides, concerns, and blockers."),
                            ),
                    )
                    .child(
                        Switch::new("settings-advisor-toggle")
                            .checked(advisor_enabled)
                            .tooltip(if advisor_enabled {
                                "Disable advisor turn watcher"
                            } else {
                                "Enable advisor turn watcher"
                            })
                            .on_click(move |checked, _window, cx| {
                                let checked = *checked;
                                advisor_toggle_model.update(cx, |state, cx| {
                                    let mut roles = state.model_roles.clone();
                                    roles.advisor_enabled = checked;
                                    state.update_model_roles(roles);
                                    cx.notify();
                                });
                            }),

                    ),
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

    fn render_scope_picker(&self, prefix: &'static str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("{prefix}-project")))
                    .icon(Icon::default().path("icons/folder.svg"))
                    .label("Project")
                    .ghost()
                    .selected(!self.install_globally)
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.install_globally = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-global")))
                    .icon(Icon::default().path("icons/globe.svg"))
                    .label("Global")
                    .ghost()
                    .selected(self.install_globally)
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.install_globally = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_capability_status(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        self.capability_status.clone().map(|status| {
            Alert::new("capability-status-alert", status)
                .title("Notice")
                .with_variant(AlertVariant::Info)
                .into_any_element()
        })
    }

    fn render_extensions(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let rows = self.extension_rows.clone();
        let project_available = self.active_project(cx).is_some();
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_capability_status(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(self.render_scope_picker("extension-scope", cx))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("extension-refresh")
                                    .icon(Icon::default().path("icons/redo.svg"))
                                    .label("Refresh")
                                    .outline()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.refresh_extensions(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("extension-install")
                                    .icon(IconName::Plus)
                                    .label("Install .wasm")
                                    .primary()
                                    .disabled(!self.install_globally && !project_available)
                                    .on_click(move |_event, _window, cx| {
                                        let Some(path) = rfd::FileDialog::new()
                                            .set_title("Install a compiled WASI extension")
                                            .add_filter("WebAssembly", &["wasm"])
                                            .pick_file()
                                        else {
                                            return;
                                        };
                                        let _ = view.update(cx, |this, cx| {
                                            let scope = if this.install_globally {
                                                ExtensionScope::Global
                                            } else {
                                                ExtensionScope::Project
                                            };
                                            let project = this.active_project(cx);
                                            this.capability_status = Some(
                                                settings::install_extension(project, &path, scope)
                                                    .unwrap_or_else(|error| error),
                                            );
                                            this.refresh_extensions(cx);
                                            this.model.update(cx, |state, cx| {
                                                state.invalidate_capability_runtimes();
                                                cx.notify();
                                            });
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            )
            .children(rows.into_iter().map(|record| {
                let toggle_record = record.clone();
                let remove_record = record.clone();
                let toggle_view = cx.entity().downgrade();
                let remove_view = cx.entity().downgrade();
                let enabled = record.is_enabled();
                let scope = match record.scope() {
                    ExtensionScope::Global => "Global",
                    ExtensionScope::Project => "Project",
                };
                let (status, status_variant) = if !enabled {
                    ("Disabled", TagVariant::Secondary)
                } else if record.is_effective() {
                    ("Active", TagVariant::Success)
                } else {
                    ("Overridden", TagVariant::Warning)
                };
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .rounded_md()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/hard-drive.svg")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{} · v{}", record.name(), record.version())),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Tag::new()
                                            .child(scope)
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(status)
                                            .with_variant(status_variant)
                                            .small(),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(record.module_path().display().to_string()),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!(
                            "extension-toggle-{}",
                            record.id()
                        )))
                        .checked(enabled)
                        .tooltip(if enabled {
                            "Disable extension"
                        } else {
                            "Enable extension"
                        })
                        .on_click(move |checked, _window, cx| {
                            let checked = *checked;
                            let _ = toggle_view.update(cx, |this, cx| {
                                let result = settings::set_extension_enabled(
                                    this.active_project(cx),
                                    &toggle_record,
                                    checked,
                                );
                                this.capability_status = result.err();
                                this.refresh_extensions(cx);
                                this.model.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                                cx.notify();
                            });
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "extension-remove-{}",
                            record.id()
                        )))
                        .icon(Icon::default().path("icons/delete.svg"))
                        .tooltip("Remove extension")
                        .ghost()
                        .w(px(32.0))
                        .h(px(32.0))
                        .on_click(move |_event, _window, cx| {
                            let _ = remove_view.update(cx, |this, cx| {
                                let result = settings::remove_extension(
                                    this.active_project(cx),
                                    &remove_record,
                                );
                                this.capability_status = result.err();
                                this.refresh_extensions(cx);
                                this.model.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                                cx.notify();
                            });
                        }),
                    )
            }))
            .when(self.extension_rows.is_empty(), |view| {
                view.child(
                    div()
                        .p_6()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No WASI extensions found."),
                )
            })
            .into_any_element()
    }

    fn render_skills(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let rows = self.skill_rows.clone();
        let has_project = self.active_project(cx).is_some();
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_capability_status(cx))
            .child(
                div().flex().justify_end().child(
                    Button::new("skills-refresh")
                        .icon(Icon::default().path("icons/redo.svg"))
                        .label("Refresh")
                        .outline()
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.refresh_skills(cx);
                            cx.notify();
                        })),
                ),
            )
            .children(rows.into_iter().map(|skill| {
                let view = cx.entity().downgrade();
                let skill_id = skill.id.clone();
                let enabled = skill.enabled;
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .rounded_md()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/book-open.svg")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(skill.name),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(skill.description),
                            )
                            .child({
                                let status_label = if !skill.is_valid {
                                    "Invalid"
                                } else if enabled {
                                    "Enabled"
                                } else {
                                    "Disabled"
                                };
                                let status_variant = if !skill.is_valid {
                                    TagVariant::Danger
                                } else if enabled {
                                    TagVariant::Success
                                } else {
                                    TagVariant::Secondary
                                };
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Tag::new()
                                            .child(skill.scope.display_name().to_string())
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(status_label)
                                            .with_variant(status_variant)
                                            .small(),
                                    )
                            }),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("skill-toggle-{skill_id}")))
                            .checked(enabled)
                            .disabled(!has_project || !skill.is_valid)
                            .tooltip(if enabled {
                                "Disable skill"
                            } else {
                                "Enable skill"
                            })
                            .on_click(move |checked, _window, cx| {
                                let checked = *checked;
                                let _ = view.update(cx, |this, cx| {
                                    let Some(project) = this.active_project(cx) else {
                                        this.capability_status =
                                            Some("Attach a project to manage skills.".into());
                                        cx.notify();
                                        return;
                                    };
                                    this.capability_status =
                                        settings::set_skill_enabled(&project, &skill_id, checked)
                                            .err();
                                    this.refresh_skills(cx);
                                    this.model.update(cx, |state, cx| {
                                        state.invalidate_capability_runtimes();
                                        cx.notify();
                                    });
                                    cx.notify();
                                });
                            }),
                    )
            }))
            .when(self.skill_rows.is_empty(), |view| {
                view.child(
                    div()
                        .p_6()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No skills found."),
                )
            })
            .into_any_element()
    }

    fn render_acp_agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let rows = self.acp_rows.clone();
        let has_project = self.active_project(cx).is_some();
        let add_view = cx.entity().downgrade();
        let name_input = self.acp_name_input.clone();
        let command_input = self.acp_command_input.clone();
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_capability_status(cx))
            .child(self.render_scope_picker("acp-scope", cx))
            .child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Input::new(&self.acp_name_input))
                    .child(Input::new(&self.acp_command_input))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("acp-refresh")
                                    .icon(Icon::default().path("icons/redo.svg"))
                                    .label("Refresh")
                                    .outline()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.refresh_acp(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("acp-add")
                                    .icon(IconName::Plus)
                                    .label("Add agent")
                                    .primary()
                                    .disabled(!self.install_globally && !has_project)
                                    .on_click(move |_event, _window, cx| {
                                        let name = name_input.read(cx).value().to_string();
                                        let command = command_input.read(cx).value().to_string();
                                        let _ = add_view.update(cx, |this, cx| {
                                            let scope = if this.install_globally {
                                                AcpScope::Global
                                            } else {
                                                AcpScope::Project
                                            };
                                            let project = this.active_project(cx);
                                            this.capability_status = settings::add_acp_agent(
                                                project.as_deref(),
                                                scope,
                                                &name,
                                                &command,
                                            )
                                            .err();
                                            this.refresh_acp(cx);
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            )
            .children(rows.into_iter().map(|record| {
                let toggle_view = cx.entity().downgrade();
                let remove_view = cx.entity().downgrade();
                let config = record.config;
                let toggle_id = config.id.clone();
                let remove_id = config.id.clone();
                let enabled = config.enabled;
                let scope = config.scope;
                let command_line = config.command_line();
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .rounded_md()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/providers/acp.svg")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(config.name),
                            )
                            .child({
                                let scope_label = match scope {
                                    AcpScope::Global => "Global",
                                    AcpScope::Project => "Project",
                                };
                                let status_label = record.status.display_status();
                                let status_variant = if status_label.contains("Ready")
                                    || status_label.contains("Available")
                                {
                                    TagVariant::Success
                                } else if status_label.contains("Failed")
                                    || status_label.contains("Error")
                                {
                                    TagVariant::Danger
                                } else {
                                    TagVariant::Info
                                };
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Tag::new()
                                            .child(scope_label)
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(status_label)
                                            .with_variant(status_variant)
                                            .small(),
                                    )
                            })
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(command_line),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("acp-toggle-{toggle_id}")))
                            .checked(enabled)
                            .tooltip(if enabled {
                                "Disable ACP agent"
                            } else {
                                "Enable ACP agent"
                            })
                            .on_click(move |checked, _window, cx| {
                                let checked = *checked;
                                let _ = toggle_view.update(cx, |this, cx| {
                                    let project = this.active_project(cx);
                                    this.capability_status = settings::set_acp_enabled(
                                        project.as_deref(),
                                        scope,
                                        &toggle_id,
                                        checked,
                                    )
                                    .err();
                                    this.refresh_acp(cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("acp-remove-{remove_id}")))
                            .icon(Icon::default().path("icons/delete.svg"))
                            .tooltip("Remove ACP agent")
                            .ghost()
                            .w(px(32.0))
                            .h(px(32.0))
                            .on_click(move |_event, _window, cx| {
                                let _ = remove_view.update(cx, |this, cx| {
                                    let project = this.active_project(cx);
                                    this.capability_status = settings::remove_acp_agent(
                                        project.as_deref(),
                                        scope,
                                        &remove_id,
                                    )
                                    .err();
                                    this.refresh_acp(cx);
                                    cx.notify();
                                });
                            }),
                    )
            }))
            .when(self.acp_rows.is_empty(), |view| {
                view.child(
                    div()
                        .p_6()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No ACP agents configured."),
                )
            })
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
            SettingsPage::Extensions => (
                "Extensions",
                "Install and manage compiled WASI extensions.",
                self.render_extensions(cx),
            ),
            SettingsPage::Skills => (
                "Skills",
                "Enable or disable skills for the active project.",
                self.render_skills(cx),
            ),
            SettingsPage::AcpAgents => (
                "ACP Agents",
                "Configure external coding agents that communicate over stdio.",
                self.render_acp_agents(cx),
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
