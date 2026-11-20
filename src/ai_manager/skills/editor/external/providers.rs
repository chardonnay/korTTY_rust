//! Edit the provider library and review an explicit test request.

use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length};

use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{Reveal, Setting, SkillProviderSetting};
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_window::CredentialChoice;

use super::java_providers::{Authentication, Hints};
use super::remote::{Provider, ProviderProfile};
use super::{Choice, MAX_QUERY_BYTES, Store, bounded};

#[derive(Clone)]
pub(crate) enum Message {
    Select(u64),
    JavaProvider(String),
    Add,
    Remove,
    Apply,
    Revert,
    Name(String),
    Type(Provider),
    Base(String),
    CredentialEnv(String),
    VaultCredential(Option<String>),
    Username(String),
    Enabled(bool),
    Sample(String),
    Test,
}

#[derive(Clone, Eq, PartialEq)]
struct ImportedChoice {
    id: String,
    label: String,
}

impl std::fmt::Display for ImportedChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

pub(super) struct Panel {
    draft: Option<ProviderProfile>,
    dirty: bool,
    sample: String,
    vault_reassignment_required: bool,
}

impl Panel {
    pub(super) fn new(store: &Store) -> Self {
        Self {
            draft: store.external_providers.first().cloned(),
            dirty: false,
            sample: String::new(),
            vault_reassignment_required: false,
        }
    }

    pub(super) fn pending_edit(&self) -> bool {
        self.dirty
    }

    pub(super) fn has_search_setting(
        &self,
        setting: SkillProviderSetting,
        store: &Store,
        busy: bool,
    ) -> bool {
        use SkillProviderSetting::*;
        if setting == Close {
            return true;
        }
        if busy {
            return false;
        }
        match setting {
            Select => !self.dirty && !store.external_providers.is_empty(),
            Add => !self.dirty && store.external_providers.len() < ProviderProfile::MAX_PROFILES,
            Remove => !self.dirty && self.draft.as_ref().is_some_and(|draft| draft.id != 0),
            Apply => self.dirty && !self.vault_reassignment_required,
            Test => self.draft.is_some() && !self.vault_reassignment_required,
            Revert => self.dirty,
            CredentialEnv => self
                .draft
                .as_ref()
                .is_some_and(|draft| draft.vault_credential.is_none()),
            ClearVaultCredential => {
                self.vault_reassignment_required
                    || self
                        .draft
                        .as_ref()
                        .is_some_and(|draft| draft.vault_credential.is_some())
            }
            Username | Sample => self
                .draft
                .as_ref()
                .is_some_and(|draft| draft.provider == Provider::Http),
            _ => self.draft.is_some(),
        }
    }

    pub(super) fn commit(&mut self, store: &mut Store) -> Result<(), String> {
        self.require_credential_choice()?;
        let Some(mut provider) = self.draft.clone().filter(|_| self.dirty) else {
            return Ok(());
        };
        if provider.id == 0 {
            if store.external_providers.len() >= ProviderProfile::MAX_PROFILES {
                return Err("The external skill provider limit has been reached.".into());
            }
            let mut next = store.clone();
            provider.id = next.next_external_provider_id()?;
            next.external_providers.push(provider.clone());
            super::remote::validate_profiles(&next.external_providers)?;
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
        java_providers: &Hints,
    ) -> Result<Option<&'static str>, String> {
        match message {
            Message::JavaProvider(id)
                if self.can_stage_import(store)
                    && ClipboardPolicy::load().ai_profile_creation_allowed() =>
            {
                let hint = java_providers
                    .get(&id)
                    .filter(|_| !Self::already_imported(store, &id))
                    .ok_or("This Java provider suggestion is unavailable.")?;
                self.draft = Some(hint.draft(&id));
                self.dirty = true;
                self.sample.clear();
                self.vault_reassignment_required = hint.authentication != Authentication::None;
                Ok(Some(
                    "Review the imported provider before applying its settings.",
                ))
            }
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
                self.vault_reassignment_required = false;
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
                self.vault_reassignment_required |= provider.vault_credential.take().is_some();
                provider.provider = kind;
                provider.base_url.clear();
                provider.credential_env.clear();
                provider.username.clear();
                self.sample.clear();
            }
            Message::Base(base) => {
                provider.base_url = bounded(base, MAX_QUERY_BYTES);
                if provider.base_url != original.base_url {
                    self.vault_reassignment_required |= provider.vault_credential.take().is_some();
                }
            }
            Message::CredentialEnv(value) if provider.vault_credential.is_none() => {
                provider.credential_env = bounded(value, 128);
                if !provider.credential_env.trim().is_empty() {
                    self.vault_reassignment_required = false;
                }
            }
            Message::VaultCredential(id) => {
                if id.is_some() {
                    provider.credential_env.clear();
                }
                if id.is_none() && provider.credential_env.is_empty() {
                    provider.username.clear();
                }
                provider.vault_credential = id;
                self.vault_reassignment_required = false;
            }
            Message::Username(value) => {
                provider.username = bounded(value, 128);
                if provider.username != original.username {
                    self.vault_reassignment_required |= provider.vault_credential.take().is_some();
                }
            }
            Message::Enabled(value) => provider.enabled = value,
            _ => {}
        }
        self.dirty |= *provider != original;
    }

    pub(super) fn test(
        &self,
        store: &Store,
    ) -> Result<(Vec<ProviderProfile>, u64, String), String> {
        self.require_credential_choice()?;
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

    fn require_credential_choice(&self) -> Result<(), String> {
        if self.vault_reassignment_required {
            return Err(
                "Choose a credential for this provider or explicitly clear authentication.".into(),
            );
        }
        Ok(())
    }

    fn can_stage_import(&self, store: &Store) -> bool {
        !self.dirty && store.external_providers.len() < ProviderProfile::MAX_PROFILES
    }

    fn already_imported(store: &Store, id: &str) -> bool {
        store
            .external_providers
            .iter()
            .any(|provider| provider.java_source.as_deref() == Some(id))
    }

    pub(super) fn view<'a>(
        &'a self,
        store: &'a Store,
        busy: bool,
        reveal: &Reveal,
        vault_credentials: &[CredentialChoice],
        java_providers: &Hints,
    ) -> Element<'a, Message> {
        use SkillProviderSetting as Target;
        let action = |setting, button| reveal.action(Setting::SkillProvider(setting), button);
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
        let fields = self
            .draft
            .as_ref()
            .map(|draft| self.fields(draft, busy, reveal, vault_credentials));
        let imported = if !busy
            && self.can_stage_import(store)
            && !java_providers.is_empty()
            && ClipboardPolicy::load().ai_profile_creation_allowed()
        {
            java_providers
                .iter()
                .filter(|(id, _)| !Self::already_imported(store, id))
                .map(|(id, hint)| ImportedChoice {
                    id: id.to_owned(),
                    label: format!("{} · {}", hint.name, hint.provider),
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let import_picker = (!imported.is_empty()).then(|| {
            pick_list(imported, None, |choice| Message::JavaProvider(choice.id))
                .placeholder("Review a Java provider suggestion…")
                .dismiss_on_escape()
        });
        column![
            text("External skill providers").size(19),
            row![
                reveal.action(Setting::SkillProvider(Target::Select), chooser),
                action(
                    Target::Add,
                    button("Add provider").on_press_maybe(
                        (!busy
                            && !self.dirty
                            && store.external_providers.len() < ProviderProfile::MAX_PROFILES)
                            .then_some(Message::Add),
                    )
                ),
                action(
                    Target::Remove,
                    button("Remove provider").on_press_maybe(
                        (!busy
                            && !self.dirty
                            && self.draft.as_ref().is_some_and(|draft| draft.id != 0))
                        .then_some(Message::Remove),
                    )
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            scrollable(column![import_picker, fields].spacing(8))
                .id(reveal.viewport.clone())
                .height(Length::Fill),
            row![
                action(
                    Target::Apply,
                    button("Apply provider edits").on_press_maybe(
                        self.has_search_setting(Target::Apply, store, busy)
                            .then_some(Message::Apply)
                    )
                ),
                action(
                    Target::Revert,
                    button("Revert provider edits")
                        .on_press_maybe((!busy && self.dirty).then_some(Message::Revert))
                ),
                action(
                    Target::Test,
                    button("Test provider").on_press_maybe(
                        self.has_search_setting(Target::Test, store, busy)
                            .then_some(Message::Test)
                    )
                ),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn fields<'a>(
        &'a self,
        draft: &'a ProviderProfile,
        busy: bool,
        reveal: &Reveal,
        vault_credentials: &[CredentialChoice],
    ) -> Element<'a, Message> {
        use SkillProviderSetting as Target;
        let input = |setting, label, placeholder, value, edit: fn(String) -> Message| {
            let setting = Setting::SkillProvider(setting);
            let editable = !busy
                && (setting != Setting::SkillProvider(Target::CredentialEnv)
                    || draft.vault_credential.is_none());
            reveal.wrap(
                setting,
                column![
                    text(label).size(13),
                    reveal.input(
                        setting,
                        editable,
                        text_input(placeholder, value).on_input_maybe(editable.then_some(edit))
                    )
                ]
                .spacing(5),
            )
        };
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
        let selected = draft
            .vault_credential
            .as_ref()
            .and_then(|id| vault_credentials.iter().find(|choice| &choice.id == id));
        let missing = if draft.vault_credential.is_some() {
            "Saved vault credential is locked or unavailable"
        } else {
            "Choose a saved vault credential"
        };
        let vault_picker: Element<'_, Message> = if busy || vault_credentials.is_empty() {
            text(selected.map_or_else(|| missing.to_owned(), |choice| choice.label.clone())).into()
        } else {
            pick_list(vault_credentials.to_vec(), selected.cloned(), |choice| {
                Message::VaultCredential(Some(choice.id))
            })
            .placeholder(missing)
            .dismiss_on_escape()
        };
        column![
            input(
                Target::Name,
                "Name",
                "Provider name",
                &draft.name,
                Message::Name
            ),
            row![
                text("Provider type").size(13),
                reveal.action(Setting::SkillProvider(Target::Type), kind),
                reveal.action(
                    Setting::SkillProvider(Target::Enabled),
                    checkbox(draft.enabled)
                        .label("Enabled")
                        .on_toggle_maybe((!busy).then_some(Message::Enabled))
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            input(
                Target::Base,
                "Base URL",
                base_placeholder,
                &draft.base_url,
                Message::Base
            ),
            input(
                Target::CredentialEnv,
                "Credential environment variable",
                "Optional variable name",
                &draft.credential_env,
                Message::CredentialEnv
            ),
            (draft.provider == Provider::Http).then(|| column![
                input(
                    Target::Username,
                    "HTTP Basic username",
                    "Optional username",
                    &draft.username,
                    Message::Username
                ),
                input(
                    Target::Sample,
                    "Test document",
                    "Markdown URL or path on this server",
                    &self.sample,
                    Message::Sample
                ),
            ]
            .spacing(5)),
            row![
                text("Vault credential").size(13),
                reveal.action(
                    Setting::SkillProvider(Target::VaultCredential),
                    vault_picker
                ),
                (draft.vault_credential.is_some() || self.vault_reassignment_required).then(|| {
                    reveal.action(
                        Setting::SkillProvider(Target::ClearVaultCredential),
                        button("Clear vault credential")
                            .on_press_maybe((!busy).then_some(Message::VaultCredential(None))),
                    )
                }),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            self.vault_reassignment_required.then(|| text(
                "Choose a credential for this provider or explicitly clear authentication."
            )
            .size(13)),
        ]
        .spacing(7)
        .width(Length::Fill)
        .into()
    }
}
