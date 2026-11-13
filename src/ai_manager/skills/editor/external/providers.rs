//! Edit the provider library and review an explicit test request.

use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length};

use crate::ai_manager::profiles::picker::DismissOnEscape as _;

use super::remote::{Provider, ProviderProfile};
use super::{Choice, MAX_QUERY_BYTES, Store, bounded};

#[derive(Clone)]
pub(crate) enum Message {
    Select(u64),
    Add,
    Remove,
    Apply,
    Revert,
    Name(String),
    Type(Provider),
    Base(String),
    CredentialEnv(String),
    Username(String),
    Enabled(bool),
    Sample(String),
    Test,
}

pub(super) struct Panel {
    draft: Option<ProviderProfile>,
    dirty: bool,
    sample: String,
}

impl Panel {
    pub(super) fn new(store: &Store) -> Self {
        Self {
            draft: store.external_providers.first().cloned(),
            dirty: false,
            sample: String::new(),
        }
    }

    pub(super) fn pending_edit(&self) -> bool {
        self.dirty
    }

    pub(super) fn commit(&mut self, store: &mut Store) -> Result<(), String> {
        let Some(mut provider) = self.draft.clone().filter(|_| self.dirty) else {
            return Ok(());
        };
        if provider.id == 0 {
            if store.external_providers.len() >= ProviderProfile::MAX_PROFILES {
                return Err("The external skill provider limit has been reached.".into());
            }
            let mut next = store.clone();
            provider.id = next.next_external_provider_id()?;
            provider.validate()?;
            next.external_providers.push(provider.clone());
            *store = next;
        } else {
            provider.validate()?;
            let current = store
                .external_providers
                .iter_mut()
                .find(|current| current.id == provider.id)
                .ok_or("The external skill provider was removed.")?;
            *current = provider.clone();
        }
        self.draft = Some(provider);
        self.dirty = false;
        Ok(())
    }

    pub(super) fn update(
        &mut self,
        store: &mut Store,
        message: Message,
    ) -> Result<Option<&'static str>, String> {
        match message {
            Message::Apply => {
                self.commit(store)?;
                Ok(Some("Provider updated in the library draft."))
            }
            Message::Select(id) if !self.dirty => {
                if let Some(provider) = store
                    .external_providers
                    .iter()
                    .find(|provider| provider.id == id)
                {
                    self.draft = Some(provider.clone());
                    self.sample.clear();
                }
                Ok(None)
            }
            Message::Add
                if !self.dirty
                    && store.external_providers.len() < ProviderProfile::MAX_PROFILES =>
            {
                self.draft = Some(ProviderProfile {
                    id: 0,
                    name: "New external skill provider".into(),
                    enabled: true,
                    ..ProviderProfile::default()
                });
                self.dirty = true;
                self.sample.clear();
                Ok(None)
            }
            Message::Remove if !self.dirty => {
                if let Some(draft) = &self.draft {
                    store
                        .external_providers
                        .retain(|provider| provider.id != draft.id);
                    *self = Self::new(store);
                    Ok(Some("Provider removed from the library draft."))
                } else {
                    Ok(None)
                }
            }
            Message::Revert => {
                self.draft = self
                    .draft
                    .as_ref()
                    .and_then(|draft| {
                        store
                            .external_providers
                            .iter()
                            .find(|provider| provider.id == draft.id)
                    })
                    .or_else(|| store.external_providers.first())
                    .cloned();
                self.dirty = false;
                self.sample.clear();
                Ok(None)
            }
            Message::Sample(value) => {
                self.sample = bounded(value, MAX_QUERY_BYTES);
                Ok(None)
            }
            message => {
                self.edit(message);
                Ok(None)
            }
        }
    }

    fn edit(&mut self, message: Message) {
        let Some(provider) = self.draft.as_mut() else {
            return;
        };
        let original = provider.clone();
        match message {
            Message::Name(name) => provider.name = bounded(name, 120),
            Message::Type(kind) if provider.provider != kind => {
                provider.provider = kind;
                provider.base_url.clear();
                provider.credential_env.clear();
                provider.username.clear();
                self.sample.clear();
            }
            Message::Base(base) => provider.base_url = bounded(base, MAX_QUERY_BYTES),
            Message::CredentialEnv(value) => provider.credential_env = bounded(value, 128),
            Message::Username(value) => provider.username = bounded(value, 128),
            Message::Enabled(value) => provider.enabled = value,
            _ => {}
        }
        self.dirty |= *provider != original;
    }

    pub(super) fn test(
        &self,
        store: &Store,
    ) -> Result<(Vec<ProviderProfile>, u64, String), String> {
        let mut profile = self.draft.clone().ok_or("Choose a provider first.")?;
        let mut next = store.clone();
        if profile.id == 0 {
            profile.id = next.next_external_provider_id()?;
        }
        profile.enabled = true;
        profile.validate()?;
        let id = profile.id;
        if let Some(saved) = next
            .external_providers
            .iter_mut()
            .find(|saved| saved.id == id)
        {
            *saved = profile;
        } else {
            next.external_providers.push(profile);
        }
        Ok((next.external_providers, id, self.sample.trim().to_owned()))
    }

    pub(super) fn view<'a>(&'a self, store: &'a Store, busy: bool) -> Element<'a, Message> {
        let choices: Vec<_> = store
            .external_providers
            .iter()
            .map(|profile| Choice {
                id: profile.id,
                name: if profile.enabled {
                    profile.name.clone()
                } else {
                    format!("Disabled · {}", profile.name)
                },
            })
            .collect();
        let selection = self
            .draft
            .as_ref()
            .and_then(|draft| choices.iter().find(|choice| choice.id == draft.id))
            .cloned();
        let chooser: Element<'_, Message> = if busy || self.dirty {
            text(
                self.draft
                    .as_ref()
                    .map_or("Choose a provider", |draft| draft.name.as_str()),
            )
            .into()
        } else {
            pick_list(choices, selection, |choice| Message::Select(choice.id))
                .placeholder("Choose a provider")
                .dismiss_on_escape()
        };
        let fields = self.draft.as_ref().map(|draft| self.fields(draft, busy));
        column![
            text("External skill providers").size(19),
            row![
                chooser,
                button("Add provider").on_press_maybe(
                    (!busy
                        && !self.dirty
                        && store.external_providers.len() < ProviderProfile::MAX_PROFILES)
                        .then_some(Message::Add),
                ),
                button("Remove provider").on_press_maybe(
                    (!busy
                        && !self.dirty
                        && self.draft.as_ref().is_some_and(|draft| draft.id != 0))
                    .then_some(Message::Remove),
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            scrollable(column![fields].spacing(8)).height(Length::Fill),
            row![
                button("Apply provider edits")
                    .on_press_maybe((!busy && self.dirty).then_some(Message::Apply)),
                button("Revert provider edits")
                    .on_press_maybe((!busy && self.dirty).then_some(Message::Revert)),
                button("Test provider")
                    .on_press_maybe((!busy && self.draft.is_some()).then_some(Message::Test)),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn fields<'a>(&'a self, draft: &'a ProviderProfile, busy: bool) -> Element<'a, Message> {
        let kind: Element<'_, Message> = if busy {
            text(draft.provider.to_string()).into()
        } else {
            pick_list(Provider::ALL, Some(draft.provider), Message::Type).dismiss_on_escape()
        };
        let base_placeholder = match draft.provider {
            Provider::GitHub => "https://api.github.com",
            Provider::SkillsMp => "https://skillsmp.com",
            Provider::Http => "Optional HTTPS base URL",
        };
        column![
            text("Name").size(13),
            text_input("Provider name", &draft.name)
                .on_input_maybe((!busy).then_some(Message::Name)),
            row![
                text("Provider type").size(13),
                kind,
                checkbox(draft.enabled)
                    .label("Enabled")
                    .on_toggle_maybe((!busy).then_some(Message::Enabled)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            text("Base URL").size(13),
            text_input(base_placeholder, &draft.base_url)
                .on_input_maybe((!busy).then_some(Message::Base)),
            text("Credential environment variable").size(13),
            text_input("Optional variable name", &draft.credential_env)
                .on_input_maybe((!busy).then_some(Message::CredentialEnv)),
            (draft.provider == Provider::Http).then(|| column![
                text("HTTP Basic username").size(13),
                text_input("Optional username", &draft.username)
                    .on_input_maybe((!busy).then_some(Message::Username)),
                text("Test document").size(13),
                text_input("Markdown URL or path on this server", &self.sample)
                    .on_input_maybe((!busy).then_some(Message::Sample)),
            ]
            .spacing(5)),
        ]
        .spacing(7)
        .width(Length::Fill)
        .into()
    }
}
