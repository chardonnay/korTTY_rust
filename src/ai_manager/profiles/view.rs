use iced::widget::{
    TextInput, button, checkbox, column, container, pick_list, responsive, row, scrollable, space,
    text, text_input,
};
use iced::{Alignment, Element, Length};

use crate::ai_manager::search::{ProfileSetting, Reveal, Setting};
use crate::clipboard_policy::{AI_PROFILE_CREATION_DISABLED, PolicyAiProfile, PolicyAiProvider};

use super::{
    ConnectionMode, Editor, Field, MAX_PROFILES, Message, ModelMode, Number, Profile, ProfileId,
    Reasoning, Section, Vision,
};
use super::{cloud, wizard};
use crate::local_ai::{self, mlx_runtime};

impl Editor {
    pub(in crate::ai_manager) fn view<'a>(
        &'a self,
        editable: bool,
        can_open_wizard: bool,
        creation_allowed: bool,
        form_lock: Option<&'static str>,
        reveal: &'a Reveal,
    ) -> Element<'a, Message> {
        let profile_actions_allowed = editable && form_lock.is_none();
        if let Some(wizard) = &self.wizard {
            return self.wizard_view(wizard, profile_actions_allowed, form_lock, reveal);
        }
        let profile_actions = row![
            button("Setup wizard").on_press_maybe(
                (can_open_wizard && self.catalog.profiles.len() < MAX_PROFILES)
                    .then_some(Message::Wizard(wizard::Action::Open))
            ),
            button("Add profile").on_press_maybe(
                (editable && creation_allowed && self.catalog.profiles.len() < MAX_PROFILES)
                    .then_some(Message::Add)
            ),
            button("Refresh")
                .style(button::secondary)
                .on_press(Message::Refresh),
            (!creation_allowed && form_lock != Some(AI_PROFILE_CREATION_DISABLED))
                .then(|| text(AI_PROFILE_CREATION_DISABLED).size(13)),
        ]
        .spacing(8)
        .wrap();
        let list = self
            .catalog
            .entries()
            .fold(column![].spacing(4), |list, profile| {
                let name = if profile.name.trim().is_empty() {
                    "Unnamed profile"
                } else {
                    profile.name.as_str()
                };
                let mut label = name.to_owned();
                if profile.policy_definition().is_some() {
                    label.push_str(" · Managed");
                }
                if self.catalog.is_marked_default(profile.id) {
                    label.push_str(" · Default");
                }
                list.push(
                    button(text(label).wrapping(text::Wrapping::WordOrGlyph))
                        .width(Length::Fill)
                        .style(if self.selected == Some(profile.id) {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .on_press(Message::Select(profile.id)),
                )
            });
        let list = scrollable(list).height(Length::Fill).width(190);
        let body: Element<'_, Message> = match self.selected() {
            Some(profile) if let Some(definition) = profile.policy_definition() => self
                .policy_profile_form(
                    profile,
                    definition,
                    profile_actions_allowed,
                    editable,
                    form_lock,
                    reveal,
                )
                .into(),
            Some(profile) => responsive(move |size| {
                self.profile_form(
                    profile,
                    profile_actions_allowed,
                    form_lock,
                    reveal,
                    size.height < 200.0,
                )
                .into()
            })
            .into(),
            None => column![
                text("No AI profiles configured"),
                creation_allowed.then(|| {
                    text("Add a profile for a local OpenAI-compatible API to get started.").size(13)
                }),
                self.status.as_deref().map(|status| text(status).size(13)),
                space::vertical().height(Length::Fill),
                row![
                    space::horizontal(),
                    button("Save").on_press_maybe(editable.then_some(Message::Save))
                ],
            ]
            .spacing(10)
            .into(),
        };
        let content = row![list, container(body).width(Length::Fill)]
            .spacing(18)
            .height(Length::Fill);
        column![profile_actions, content]
            .spacing(10)
            .height(Length::Fill)
            .into()
    }

    fn policy_profile_form<'a>(
        &'a self,
        profile: &'a Profile,
        definition: &'a PolicyAiProfile,
        actions_allowed: bool,
        can_read_usage: bool,
        form_lock: Option<&'static str>,
        reveal: &Reveal,
    ) -> iced::widget::Column<'a, Message> {
        let provider = match definition.provider {
            PolicyAiProvider::Anthropic => "Anthropic",
            PolicyAiProvider::OpenAiCompatible => "OpenAI-compatible",
            PolicyAiProvider::LmStudio => "LM Studio",
            PolicyAiProvider::EmbeddedLlama => "Embedded llama.cpp",
            PolicyAiProvider::EmbeddedMlx => "Embedded Apple MLX",
        };
        let destination = profile
            .destination()
            .unwrap_or_else(|_| "Unavailable".into());
        let credential = if definition.encrypted_key.is_some() {
            "•••••••• · Provided by your organization"
        } else {
            "None configured"
        };
        let automatic = local_ai::Profile::supports_policy_auto_model(definition);
        let can_discover = !automatic
            && definition.model.is_none()
            && matches!(
                definition.provider,
                PolicyAiProvider::Anthropic
                    | PolicyAiProvider::OpenAiCompatible
                    | PolicyAiProvider::LmStudio
            );
        let discovered = (can_discover && !self.models.is_empty()).then(|| {
            let models = column(
                self.models
                    .iter()
                    .take(5)
                    .map(|model| text(model).wrapping(text::Wrapping::WordOrGlyph).into()),
            )
            .spacing(4);
            policy_field(
                reveal,
                ProfileSetting::AvailableModels,
                "Discovered models",
                column![
                    text(format!("{} available", self.models.len())).size(13),
                    models
                ]
                .spacing(4),
            )
        });
        let id = profile.id;
        let default = self.catalog.is_marked_default(id);
        let is_default = checkbox(default)
            .label(ProfileSetting::Default.title())
            .on_toggle_maybe(
                (actions_allowed && self.can_set_default(id))
                    .then_some(move |_| Message::SetDefault(id)),
            );
        let details = column![
            text("Managed by your organization").size(13),
            policy_field(
                reveal,
                ProfileSetting::Name,
                "Profile name",
                text(&profile.name).wrapping(text::Wrapping::WordOrGlyph)
            ),
            policy_field(
                reveal,
                ProfileSetting::Connection,
                "Provider",
                text(provider)
            ),
            policy_field(
                reveal,
                ProfileSetting::Endpoint,
                "Destination",
                text(destination).wrapping(text::Wrapping::WordOrGlyph)
            ),
            policy_field(
                reveal,
                ProfileSetting::Model,
                "Model",
                text(if !profile.model.is_empty() {
                    profile.model.as_str()
                } else if automatic {
                    "Automatic · LM Studio"
                } else {
                    "None configured"
                })
                .wrapping(text::Wrapping::WordOrGlyph)
            ),
            policy_field(
                reveal,
                ProfileSetting::Credential,
                "API key",
                text(credential).wrapping(text::Wrapping::WordOrGlyph)
            ),
            discovered,
            profile
                .policy_auto_problem()
                .map(|problem| text(problem).size(13).wrapping(text::Wrapping::WordOrGlyph)),
            reveal.wrap(Setting::Profile(ProfileSetting::Default), is_default),
            self.usage.recorded(profile, can_read_usage),
            form_lock.map(|notice| text(notice).size(13)),
            self.status.as_deref().map(|status| text(status).size(13)),
        ]
        .spacing(8);
        let action = if can_discover {
            button("Discover models").on_press_maybe(actions_allowed.then_some(Message::Discover))
        } else {
            button("Test connection").on_press_maybe(
                (actions_allowed && (definition.model.is_some() || automatic))
                    .then_some(Message::Test),
            )
        };
        column![
            scrollable(details)
                .id(reveal.viewport.clone())
                .height(Length::Fill),
            action.style(button::secondary),
        ]
        .spacing(10)
    }

    fn profile_form<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        form_lock: Option<&'static str>,
        reveal: &Reveal,
        compact: bool,
    ) -> iced::widget::Column<'a, Message> {
        let default = self.catalog.is_marked_default(profile.id);
        let name = text_input(ProfileSetting::Name.title(), &profile.name)
            .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Name, value)));
        let mut is_default = checkbox(default).label(ProfileSetting::Default.title());
        if editable && self.can_set_default(profile.id) {
            let id = profile.id;
            is_default = is_default.on_toggle(move |_| Message::SetDefault(id));
        }
        let header = row![
            reveal.wrap(
                Setting::Profile(ProfileSetting::Name),
                reveal.input(Setting::Profile(ProfileSetting::Name), editable, name),
            ),
            (!profile.is_cli())
                .then(|| reveal.wrap(Setting::Profile(ProfileSetting::Default), is_default)),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let sections = row![
            section("Connection", Section::Connection, self.section),
            section("Model", Section::Model, self.section),
            section("Limits and timeouts", Section::Limits, self.section),
        ]
        .spacing(6)
        .wrap();
        let form = match self.section {
            Section::Connection => column![
                self.connection(profile, editable, reveal),
                (!profile.is_cli()).then(|| self.web_search_view(profile, editable)),
            ]
            .spacing(10)
            .into(),
            Section::Model => self.model(profile, editable, reveal),
            Section::Limits => self.limits(profile, editable, reveal),
        };
        let actions = if self.deleting {
            row![
                text("Remove this profile from the draft?").width(Length::Fill),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Message::CancelDelete),
                button("Remove profile")
                    .style(button::danger)
                    .on_press_maybe(editable.then_some(Message::ConfirmDelete)),
            ]
        } else {
            row![
                button(if profile.is_cli() {
                    "Run fixed CLI test"
                } else {
                    "Test connection"
                })
                .style(button::secondary)
                .on_press_maybe(editable.then_some(Message::Test)),
                space::horizontal(),
                button("Delete")
                    .style(button::danger)
                    .on_press_maybe(editable.then_some(Message::Delete)),
                button("Save").on_press_maybe(editable.then_some(Message::Save)),
            ]
        }
        .spacing(8)
        .align_y(Alignment::Center);
        let header = column![
            header,
            form_lock.map(|notice| text(notice).size(13)),
            self.status.as_deref().map(|status| text(status).size(13)),
            sections,
        ]
        .spacing(10);
        let form = container(form).padding(4);
        let (editor, content): (_, Element<'a, Message>) = if compact {
            (column![].spacing(10), header.push(form).into())
        } else {
            (header, form.into())
        };
        editor
            .push(
                scrollable(content)
                    .id(reveal.viewport.clone())
                    .height(Length::Fill),
            )
            .push(actions)
    }

    pub(in crate::ai_manager::profiles) fn connection<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let mode = if mlx_runtime::supported() || profile.is_mlx() {
            choice(
                ConnectionMode::WITH_MLX,
                profile.connection_mode,
                editable,
                Message::ConnectionMode,
            )
        } else {
            choice(
                ConnectionMode::ALL,
                profile.connection_mode,
                editable,
                Message::ConnectionMode,
            )
        };
        let mode = field(reveal, ProfileSetting::Connection, mode);
        if profile.is_cli() {
            return column![mode, self.cli_connection(profile, editable)]
                .spacing(10)
                .into();
        }
        if profile.is_managed() {
            return column![
                mode,
                text(if profile.is_mlx() {
                    "This profile starts the installed Apple MLX runtime on this computer. Import a text model in Local Models."
                } else {
                    "This profile starts the installed llama.cpp runtime on this computer. Select a GGUF model imported in Local Models."
                }).size(13),
            ]
            .spacing(10)
            .into();
        }
        if let Some(protocol) = profile.remote_protocol() {
            let destination = profile.destination().ok();
            let credential_editable = editable && profile.trusted_remote_endpoint;
            let provider_picker = editable.then(|| cloud_provider_picker(profile));
            return column![
                mode,
                provider_picker,
                text_field(reveal, ProfileSetting::Endpoint, editable, text_input(match protocol {
                    crate::local_ai::RemoteProtocol::OpenAi => "https://api.openai.com/v1",
                    crate::local_ai::RemoteProtocol::Anthropic => "https://api.anthropic.com/v1",
                }, &profile.endpoint).on_input_maybe(editable.then_some(|value| Message::Edit(Field::Endpoint, value)))),
                destination.as_ref().map(|url| text(format!("Request destination · {url}")).size(13)),
                field(reveal, ProfileSetting::Trust, checkbox(profile.trusted_remote_endpoint)
                    .label("Trust this HTTPS destination to receive reviewed AI requests")
                    .on_toggle_maybe((editable && destination.is_some()).then_some(Message::TrustRemote))),
                text_field(reveal, ProfileSetting::Credential, credential_editable, text_input("API key environment variable name", &profile.credential_env)
                    .on_input_maybe(credential_editable.then_some(|value| Message::Edit(Field::Credential, value)))),
                text("Requests send reviewed content outside this computer. Certificate verification uses the system trust store. The profile stores the credential variable name only.").size(13),
            ].spacing(10).into();
        }
        let mut trusted = checkbox(profile.trusted_local_endpoint)
            .label("Trust this local endpoint with an API key");
        if editable {
            trusted = trusted.on_toggle(Message::TrustLocal);
        }
        let credential_editable = editable && profile.trusted_local_endpoint;
        column![
            mode,
            text_field(reveal, ProfileSetting::Endpoint, editable, text_input(if profile.connection_mode == ConnectionMode::LmStudio { "127.0.0.1:1234" } else { "127.0.0.1:11434" }, &profile.endpoint)
                .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Endpoint, value)))),
            field(reveal, ProfileSetting::Trust, trusted),
            text_field(reveal, ProfileSetting::Credential, credential_editable, text_input("Environment variable name (optional)", &profile.credential_env)
                .on_input_maybe(credential_editable.then_some(|value| Message::Edit(Field::Credential, value)))),
            text("Store only the name of the environment variable here. A loopback address can also point to a tunnel or proxy. Enable trust only if this endpoint runs on this computer.").size(13),
            (profile.connection_mode == ConnectionMode::LmStudio).then(|| text("Uses /api/v1/chat with server-side conversation storage disabled. Reviewed conversation history is included as an ordered transcript.").size(13)),
        ]
        .spacing(10)
        .into()
    }

    pub(in crate::ai_manager::profiles) fn model<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let content = if profile.is_cli() {
            text("Configure the optional model identifier and arguments in Connection. CLI profiles accept text prompts and read the answer from stdout.").size(13).into()
        } else if profile.is_managed() {
            self.managed_model(profile, editable, reveal)
        } else {
            self.external_model(profile, editable, reveal)
        };
        let preset = profile.prompt_preset.unwrap_or_default();
        column![
            content,
            field(
                reveal,
                ProfileSetting::PromptPreset,
                choice(
                    local_ai::PromptPreset::ALL,
                    preset,
                    editable,
                    Message::PromptPreset,
                ),
            ),
            (preset == local_ai::PromptPreset::Auto).then(|| text(format!(
                "Auto uses {} for this model identifier.",
                profile.effective_prompt_preset_in(
                    self.model_catalog.as_deref(),
                    self.managed_model_name(profile)
                )
            ))
            .size(13)),
        ]
        .spacing(10)
        .into()
    }

    fn external_model<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let selection = choice(
            ModelMode::ALL,
            profile.model_mode,
            editable,
            Message::ModelMode,
        );
        let choices = cloud::model_choices(profile, &self.models);
        let has_choices = !choices.is_empty();
        let discovered = super::picker::escape_dismissible(
            pick_list(choices, None::<String>, |model| {
                Message::Edit(Field::Model, model)
            })
            .placeholder("Choose a model"),
        );
        let model_input = text_input("Model identifier", &profile.model)
            .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Model, value)));
        let mut model = column![field(reveal, ProfileSetting::ModelMode, selection)].spacing(10);
        if profile.model_mode == ModelMode::Manual {
            model = model.push(text_field(
                reveal,
                ProfileSetting::Model,
                editable,
                model_input,
            ));
        } else {
            let model_text = if profile.model.is_empty() {
                "Load models to resolve the first available model".into()
            } else {
                format!("Resolved model: {}", profile.model)
            };
            model = model.push(text(model_text).size(13));
        }
        model
            .push(field(
                reveal,
                ProfileSetting::AvailableModels,
                row![
                    (editable && has_choices && profile.model_mode == ModelMode::Manual)
                        .then_some(discovered),
                    button("Load models")
                        .style(button::secondary)
                        .on_press_maybe(editable.then_some(Message::Discover)),
                ]
                .spacing(6)
                .wrap(),
            ))
            .push(self.reasoning(profile, editable, reveal))
            .push(field(
                reveal,
                ProfileSetting::Vision,
                choice(Vision::ALL, profile.vision, editable, Message::Vision),
            ))
            .into()
    }

    fn managed_model<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let models = self.managed_catalog(profile);
        let selected = models
            .choices
            .iter()
            .find(|choice| choice.id == profile.model);
        let model: Element<'_, Message> = if editable && !models.loading {
            super::picker::escape_dismissible(
                pick_list(models.choices.as_slice(), selected, |choice| {
                    Message::ManagedModel(choice.id)
                })
                .placeholder("Choose a managed model"),
            )
        } else {
            text(selected.map_or_else(|| profile.model.clone(), ToString::to_string)).into()
        };
        let status = if models.loading {
            "Loading managed models…"
        } else if let Some(problem) = &models.problem {
            problem.as_str()
        } else if selected.is_none() && super::valid_policy_model_id(&profile.model) {
            "The saved policy-managed model is unavailable. Refresh after the installed policy or source is updated."
        } else if selected.is_none() && !profile.model.is_empty() {
            "The saved managed model is unavailable. Select another managed model."
        } else if models.policy_model_unavailable {
            "One or more policy-managed models are unavailable. Refresh after the installed policy or source is updated."
        } else if models.policy_only {
            "Your organization allows only policy-managed models for embedded AI profiles."
        } else if profile.is_mlx() {
            "Import a data-only MLX safetensors directory in Local Models."
        } else {
            "Import GGUF files in Local Models. A registered projector enables vision."
        };
        column![
            field(
                reveal,
                ProfileSetting::ManagedModel,
                row![
                    model,
                    button("Refresh models")
                        .style(button::secondary)
                        .on_press_maybe(
                            (editable && !models.loading).then_some(Message::RefreshManagedModels)
                        ),
                ]
                .spacing(6)
                .wrap(),
            ),
            text(status).size(13),
            selected.filter(|choice| choice.vision).map(|_| field(
                reveal,
                ProfileSetting::Vision,
                if editable {
                    super::picker::escape_dismissible(pick_list(
                        [Vision::Disabled, Vision::Enabled],
                        Some(profile.vision),
                        Message::Vision,
                    ))
                } else {
                    text(profile.vision.to_string()).into()
                },
            )),
            (!profile.is_mlx()).then(|| self.reasoning(profile, editable, reveal)),
        ]
        .spacing(10)
        .into()
    }

    fn limits<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let override_row = |setting, number, value: Option<u32>, default| {
            let mut toggle = checkbox(value.is_some()).label("Override");
            if editable {
                toggle = toggle.on_toggle(move |enabled| Message::Override(number, enabled));
            }
            let input_editable = editable && value.is_some();
            let input = self.number(profile.id, number, value.unwrap_or(default), input_editable);
            let input = reveal.input(Setting::Profile(setting), input_editable, input);
            field(
                reveal,
                setting,
                row![toggle, input].spacing(8).align_y(Alignment::Center),
            )
        };
        column![
            text_field(reveal, ProfileSetting::SelectionLimit, editable, self.number(profile.id, Number::Selection, profile.selection_limit, editable)),
            override_row(ProfileSetting::Timeout, Number::Timeout, profile.timeout_minutes, 1),
            (!profile.is_cli()).then(|| override_row(ProfileSetting::OutputLimit, Number::OutputTokens, profile.max_output_tokens, 16_000)),
            (!profile.is_cli()).then(|| field(
                reveal,
                ProfileSetting::Tokenizer,
                choice(crate::local_ai::Tokenizer::ALL, profile.tokenizer, editable, Message::Tokenizer),
            )),
            (profile.remote_protocol().is_some() && profile.policy_definition().is_none()).then(||
                checkbox(profile.allows_reviewed_agent_https())
                    .label("Allow reviewed Agent, Swarm, and Scheduler requests over HTTPS")
                    .on_toggle_maybe((editable && profile.trusted_remote_endpoint).then_some(Message::ReviewedAgentHttps))
            ),
            text(if profile.is_cli() {
                "CLI requests inherit the timeout set in General and have a 30-minute maximum. The fixed CLI test uses 30 seconds."
            } else {
                "Profiles inherit the timeout set in General. An override of zero uses the transport’s default timeout."
            }).size(13),
            (!profile.is_cli()).then(|| self.usage.view(profile, editable)),
        ]
        .spacing(10)
        .into()
    }

    fn number(
        &self,
        id: ProfileId,
        number: Number,
        value: u32,
        editable: bool,
    ) -> iced::widget::TextInput<'_, Message> {
        let fallback = value.to_string();
        let value = self
            .invalid
            .get(&(id, number))
            .map_or(fallback.as_str(), String::as_str);
        text_input("Enter a number", value)
            .width(125)
            .on_input_maybe(editable.then_some(move |value| Message::Number(number, value)))
    }

    fn reasoning(
        &self,
        profile: &Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'static, Message> {
        field(
            reveal,
            ProfileSetting::Reasoning,
            choice(
                Reasoning::ALL,
                profile.reasoning,
                editable,
                Message::Reasoning,
            ),
        )
    }
}

fn section(
    label: &'static str,
    target: Section,
    active: Section,
) -> iced::widget::Button<'static, Message> {
    button(label)
        .style(if active == target {
            button::primary
        } else {
            button::secondary
        })
        .on_press(Message::Section(target))
}

fn field<'a>(
    reveal: &Reveal,
    setting: ProfileSetting,
    input: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    reveal.wrap(
        Setting::Profile(setting),
        row![text(setting.title()).width(172).size(13), input.into()]
            .spacing(8)
            .align_y(Alignment::Center),
    )
}

fn policy_field<'a>(
    reveal: &Reveal,
    setting: ProfileSetting,
    label: &'static str,
    value: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    reveal.wrap(
        Setting::Profile(setting),
        row![text(label).width(112).size(13), value.into()].spacing(8),
    )
}

fn text_field<'a>(
    reveal: &Reveal,
    setting: ProfileSetting,
    editable: bool,
    input: TextInput<'a, Message>,
) -> Element<'a, Message> {
    field(
        reveal,
        setting,
        reveal.input(Setting::Profile(setting), editable, input),
    )
}

pub(super) fn cloud_provider_picker(profile: &Profile) -> Element<'static, Message> {
    let selected = cloud::Provider::ALL
        .into_iter()
        .find(|provider| provider.matches_endpoint(profile));
    row![
        text("Cloud provider").size(13),
        super::picker::escape_dismissible(
            pick_list(cloud::Provider::ALL, selected, Message::CloudProvider)
                .placeholder("Choose a provider preset")
        ),
    ]
    .spacing(8)
    .into()
}

pub(super) fn choice<T: ToString + PartialEq + Clone + 'static, const N: usize>(
    options: [T; N],
    current: T,
    editable: bool,
    select: impl Fn(T) -> Message + 'static,
) -> Element<'static, Message> {
    if editable {
        super::picker::escape_dismissible(pick_list(options, Some(current), select))
    } else {
        text(current.to_string()).into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
    use iced::{Event, Font, Renderer, Size, mouse};

    use super::{super::Catalog, *};

    #[test]
    fn managed_ai_profile_form_exposes_only_permitted_local_actions() {
        let definition = Arc::new(PolicyAiProfile {
            id: "policy-fixture".into(),
            name: "Managed fixture".into(),
            provider: PolicyAiProvider::OpenAiCompatible,
            endpoint: Some("https://example.invalid/v1".into()),
            model: Some("example-model".into()),
            encrypted_key: Some("fixture-ciphertext".into()),
        });
        let managed_id = 2;
        let editor = Editor {
            catalog: Catalog {
                profiles: vec![Profile {
                    id: 1,
                    ..Profile::default()
                }],
                default: Some(1),
                last_id: 1,
                policy_profiles: vec![Profile {
                    id: managed_id,
                    name: definition.name.clone(),
                    policy: Some(Arc::clone(&definition)),
                    ..Profile::default()
                }],
                ..Catalog::default()
            },
            selected: Some(managed_id),
            ..Editor::default()
        };
        let reveal = Reveal::default();
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut automatic = (*definition).clone();
        automatic.model = None;
        let mut loaded = automatic.clone();
        loaded.endpoint = Some("http://127.0.0.1:1234/v1".into());
        for (definition, allowed, can_read_usage, height) in [
            (definition.as_ref(), true, true, 320.0),
            (definition.as_ref(), false, true, 500.0),
            (definition.as_ref(), false, false, 500.0),
            (&automatic, true, true, 320.0),
            (&automatic, false, true, 320.0),
            (&loaded, true, true, 320.0),
            (&loaded, false, true, 320.0),
        ] {
            let mut view: Element<'_, Message> = editor
                .policy_profile_form(
                    editor.selected().unwrap(),
                    definition,
                    allowed,
                    can_read_usage,
                    None,
                    &reveal,
                )
                .into();
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(460.0, height)),
            );
            let layout = Layout::new(&node);
            let mut children = layout.children();
            let details = children.next().unwrap().children().next().unwrap();
            let detail_rows: Vec<_> = details.children().collect();
            let [.., default, recorded_usage] = detail_rows.as_slice() else {
                panic!("expected separate default and usage controls");
            };
            let default = default.children().next().unwrap();
            let recorded_usage = recorded_usage.children().next().unwrap();
            let test = children.next().unwrap();
            assert!(children.next().is_none());
            let action_count = if height == 320.0 { 2 } else { 3 };
            for (index, target) in [default, test, recorded_usage]
                .into_iter()
                .take(action_count)
                .enumerate()
            {
                let cursor = mouse::Cursor::Available(target.bounds().center());
                let mut messages = Vec::new();
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    view.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(event),
                        layout,
                        cursor,
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &layout.bounds(),
                    );
                }
                assert!(match (allowed, index) {
                    (_, 2) if can_read_usage => matches!(messages.as_slice(), [Message::LoadUsage]),
                    (_, 2) => messages.is_empty(),
                    (true, 0) =>
                        matches!(messages.as_slice(), [Message::SetDefault(id)] if *id == managed_id),
                    (true, 1)
                        if definition.model.is_none()
                            && !local_ai::Profile::supports_policy_auto_model(definition) =>
                    {
                        matches!(messages.as_slice(), [Message::Discover])
                    }
                    (true, 1) => matches!(messages.as_slice(), [Message::Test]),
                    (false, _) => messages.is_empty(),
                    _ => false,
                });
            }
        }
    }
}
