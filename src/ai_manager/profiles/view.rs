use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length};

use super::wizard;
use super::{
    Editor, Field, MAX_PROFILES, Message, ModelMode, Number, Profile, Reasoning, Section, Vision,
};

impl Editor {
    pub(in crate::ai_manager) fn view(&self, editable: bool) -> Element<'_, Message> {
        if let Some(wizard) = &self.wizard {
            return self.wizard_view(wizard, editable);
        }
        let save = button("Save").on_press_maybe(editable.then_some(Message::Save));
        let profile_actions = row![
            button("Setup wizard").on_press_maybe(
                (editable && self.catalog.profiles.len() < MAX_PROFILES)
                    .then_some(Message::Wizard(wizard::Action::Open))
            ),
            button("Add profile").on_press_maybe(
                (editable && self.catalog.profiles.len() < MAX_PROFILES).then_some(Message::Add)
            ),
            button("Refresh")
                .style(button::secondary)
                .on_press(Message::Refresh),
        ]
        .spacing(8)
        .wrap();
        let list = self
            .catalog
            .profiles
            .iter()
            .fold(column![].spacing(4), |list, profile| {
                let name = if profile.name.trim().is_empty() {
                    "Unnamed profile"
                } else {
                    profile.name.as_str()
                };
                let label = if self.catalog.default == Some(profile.id) {
                    format!("{name} · Default")
                } else {
                    name.to_owned()
                };
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
        let left = column![profile_actions, scrollable(list).height(Length::Fill)]
            .spacing(10)
            .width(190);
        let body = match self.selected() {
            Some(profile) => self.profile_form(profile, editable, save),
            None => column![
                text("No AI profiles configured"),
                text("Add a profile for a local OpenAI-compatible API to get started.").size(13),
                self.status.as_deref().map(|status| text(status).size(13)),
                space::vertical().height(Length::Fill),
                row![space::horizontal(), save],
            ]
            .spacing(10),
        };
        row![left, body.width(Length::Fill)]
            .spacing(18)
            .height(Length::Fill)
            .into()
    }

    fn profile_form<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        save: iced::widget::Button<'a, Message>,
    ) -> iced::widget::Column<'a, Message> {
        let default = self.catalog.default == Some(profile.id);
        let name = text_input("Profile name", &profile.name)
            .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Name, value)));
        let mut is_default = checkbox(default).label("Use as default profile");
        if editable && !default {
            is_default = is_default.on_toggle(|_| Message::SetDefault);
        }
        let header = row![name, is_default]
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
            Section::Connection => self.connection(profile, editable),
            Section::Model => self.model(profile, editable),
            Section::Limits => self.limits(profile, editable),
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
                button("Test connection")
                    .style(button::secondary)
                    .on_press_maybe(editable.then_some(Message::Test)),
                space::horizontal(),
                button("Delete")
                    .style(button::danger)
                    .on_press_maybe(editable.then_some(Message::Delete)),
                save,
            ]
        }
        .spacing(8)
        .align_y(Alignment::Center);
        column![
            header,
            self.status.as_deref().map(|status| text(status).size(13)),
            sections,
            scrollable(container(form).padding(4)).height(Length::Fill),
            actions,
        ]
        .spacing(10)
    }

    pub(in crate::ai_manager::profiles) fn connection<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
    ) -> Element<'a, Message> {
        let mut trusted = checkbox(profile.trusted_local_endpoint)
            .label("Trust this local endpoint with an API key");
        if editable {
            trusted = trusted.on_toggle(Message::TrustLocal);
        }
        column![
            field("Connection type", text("Local OpenAI-compatible API")),
            field("API endpoint", text_input("127.0.0.1:11434", &profile.endpoint)
                .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Endpoint, value)))),
            field("Endpoint trust", trusted),
            field("API key variable", text_input("Environment variable name (optional)", &profile.credential_env)
                .on_input_maybe((editable && profile.trusted_local_endpoint).then_some(|value| Message::Edit(Field::Credential, value)))),
            text("Store only the name of the environment variable here. A loopback address can also point to a tunnel or proxy. Enable trust only if this endpoint runs on this computer.").size(13),
        ]
        .spacing(10)
        .into()
    }

    pub(in crate::ai_manager::profiles) fn model<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
    ) -> Element<'a, Message> {
        let selection = choice(
            ModelMode::ALL,
            profile.model_mode,
            editable,
            Message::ModelMode,
        );
        let discovered = pick_list(self.models.as_slice(), None::<&String>, |model| {
            Message::Edit(Field::Model, model)
        })
        .placeholder("Choose discovered model");
        let model_input = text_input("Model identifier", &profile.model)
            .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Model, value)));
        let mut model = column![field("Model selection", selection)].spacing(10);
        if profile.model_mode == ModelMode::Manual {
            model = model.push(field("Model identifier", model_input));
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
                "Available models",
                row![
                    (editable
                        && !self.models.is_empty()
                        && profile.model_mode == ModelMode::Manual)
                        .then_some(discovered),
                    button("Load models")
                        .style(button::secondary)
                        .on_press_maybe(editable.then_some(Message::Discover)),
                ]
                .spacing(6)
                .wrap(),
            ))
            .push(field(
                "Reasoning",
                choice(
                    Reasoning::ALL,
                    profile.reasoning,
                    editable,
                    Message::Reasoning,
                ),
            ))
            .push(field(
                "Vision",
                choice(Vision::ALL, profile.vision, editable, Message::Vision),
            ))
            .into()
    }

    fn limits<'a>(&'a self, profile: &'a Profile, editable: bool) -> Element<'a, Message> {
        let override_row = |label, number, value: Option<u32>, default| {
            let mut toggle = checkbox(value.is_some()).label("Override");
            if editable {
                toggle = toggle.on_toggle(move |enabled| Message::Override(number, enabled));
            }
            let input = self.number(
                profile.id,
                number,
                value.unwrap_or(default),
                editable && value.is_some(),
            );
            field(
                label,
                row![toggle, input].spacing(8).align_y(Alignment::Center),
            )
        };
        column![
            field("Max. selection characters", self.number(profile.id, Number::Selection, profile.selection_limit, editable)),
            override_row("Profile timeout (min)", Number::Timeout, profile.timeout_minutes, 1),
            override_row("Max. output tokens", Number::OutputTokens, profile.max_output_tokens, 16_000),
            text("Profiles inherit the timeout set in General. An override of zero uses the transport’s default timeout.").size(13),
        ]
        .spacing(10)
        .into()
    }

    fn number(
        &self,
        id: u64,
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
    label: &'static str,
    input: impl Into<Element<'a, Message>>,
) -> iced::widget::Row<'a, Message> {
    row![text(label).width(172).size(13), input.into()]
        .spacing(8)
        .align_y(Alignment::Center)
}

fn choice<T: ToString + PartialEq + Clone + 'static, const N: usize>(
    options: [T; N],
    current: T,
    editable: bool,
    select: impl Fn(T) -> Message + 'static,
) -> Element<'static, Message> {
    if editable {
        pick_list(options, Some(current), select).into()
    } else {
        text(current.to_string()).into()
    }
}
