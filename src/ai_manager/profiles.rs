//! Saved AI profiles and the draft owned by the native manager.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::local_ai::{self, ProfileOptions};

mod view;
mod wizard;

use wizard::Wizard;

const MAX_PROFILES: usize = 128;
const MAX_NAME_BYTES: usize = 160;
const MAX_ENDPOINT_BYTES: usize = 2048;
const MAX_TIMEOUT_MINUTES: u32 = 1440;
const MAX_SELECTION_CHARS: u32 = 50_000_000;

#[derive(Clone, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Catalog {
    profiles: Vec<Profile>,
    default: Option<u64>,
    last_id: u64,
}

impl Catalog {
    pub(super) fn entries(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.iter()
    }

    pub(super) fn choices(&self) -> Vec<(u64, String)> {
        self.profiles
            .iter()
            .map(|profile| (profile.id, profile.name.clone()))
            .collect()
    }

    pub(super) fn profile(&self, id: u64) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub(super) fn default_profile(&self) -> Option<&Profile> {
        self.default.and_then(|id| self.profile(id))
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        let valid_default = match self.default {
            Some(_) => self.default_profile().is_some(),
            None => self.profiles.is_empty(),
        };
        if self.profiles.len() > MAX_PROFILES || !valid_default {
            return Err("Invalid AI profile catalog".into());
        }
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            if profile.id == 0 || profile.id > self.last_id || !ids.insert(profile.id) {
                return Err("Invalid AI profile identifier".into());
            }
            profile.validate()?;
        }
        Ok(())
    }
}

impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AI profile catalog")
            .field("profile_count", &self.profiles.len())
            .field("default", &self.default)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Profile {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) endpoint: String,
    pub(super) model: String,
    pub(super) credential_env: String,
    pub(super) trusted_local_endpoint: bool,
    pub(super) model_mode: ModelMode,
    pub(super) vision: Vision,
    pub(super) reasoning: Reasoning,
    pub(super) selection_limit: u32,
    pub(super) timeout_minutes: Option<u32>,
    pub(super) max_output_tokens: Option<u32>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            endpoint: local_ai::DEFAULT_ENDPOINT.into(),
            model: String::new(),
            credential_env: String::new(),
            trusted_local_endpoint: false,
            model_mode: ModelMode::Manual,
            vision: Vision::Auto,
            reasoning: Reasoning::Default,
            selection_limit: 1_000_000,
            timeout_minutes: None,
            max_output_tokens: None,
        }
    }
}

impl Profile {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || !plain_text(&self.name, MAX_NAME_BYTES) {
            return Err("Enter a profile name of up to 160 bytes".into());
        }
        if !plain_text(&self.endpoint, MAX_ENDPOINT_BYTES) {
            return Err("Enter a valid local API endpoint".into());
        }
        local_ai::numeric_loopback_endpoint(&self.endpoint)?;
        if !plain_text(&self.model, local_ai::MAX_MODEL_BYTES)
            || self.model.trim().chars().any(char::is_whitespace)
        {
            return Err("Enter a valid model identifier".into());
        }
        self.credential()?;
        if !(1..=MAX_SELECTION_CHARS).contains(&self.selection_limit)
            || self
                .timeout_minutes
                .is_some_and(|minutes| minutes > MAX_TIMEOUT_MINUTES)
            || self
                .max_output_tokens
                .is_some_and(|tokens| !(256..=1_048_576).contains(&tokens))
        {
            return Err("One or more AI profile limits are out of range".into());
        }
        Ok(())
    }

    pub(super) fn credential(&self) -> Result<Option<String>, String> {
        let name = self.credential_env.trim();
        if name.is_empty() {
            return Ok(None);
        }
        if !self.trusted_local_endpoint {
            return Err("Trust the local endpoint before adding an API key reference".into());
        }
        if !valid_env_name(name) {
            return Err("Enter the name of an environment variable for the API key".into());
        }
        Ok(Some(name.into()))
    }

    pub(super) fn transport(&self, default_timeout: u16) -> Result<local_ai::Profile, String> {
        self.transport_with_model(default_timeout, &self.model)
    }

    pub(super) fn transport_with_model(
        &self,
        default_timeout: u16,
        model: &str,
    ) -> Result<local_ai::Profile, String> {
        let minutes = self.timeout_minutes.unwrap_or(u32::from(default_timeout));
        local_ai::Profile::configured(
            &self.endpoint,
            model,
            ProfileOptions {
                max_output_tokens: self.max_output_tokens,
                timeout: (minutes != 0).then(|| Duration::from_secs(u64::from(minutes) * 60)),
                reasoning_effort: self.reasoning.api_value().map(str::to_owned),
                credential_env: self.credential()?,
            },
        )
    }
}

fn plain_text(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes
            .next()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ModelMode {
    Manual,
    Auto,
}

impl ModelMode {
    const ALL: [Self; 2] = [Self::Manual, Self::Auto];
}

impl fmt::Display for ModelMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manual => "Manual",
            Self::Auto => "Automatic",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Vision {
    Auto,
    Enabled,
    Disabled,
}

impl Vision {
    const ALL: [Self; 3] = [Self::Auto, Self::Enabled, Self::Disabled];
}

impl fmt::Display for Vision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "Automatic",
            Self::Enabled => "Enabled",
            Self::Disabled => "Disabled",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Reasoning {
    Default,
    None,
    Minimal,
    Low,
    Medium,
    High,
    ExtraHigh,
}

impl Reasoning {
    const ALL: [Self; 7] = [
        Self::Default,
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::ExtraHigh,
    ];

    pub(super) fn api_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::None => Some("none"),
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::ExtraHigh => Some("xhigh"),
        }
    }
}

impl fmt::Display for Reasoning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Default => "Model default",
            Self::None => "None",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::ExtraHigh => "Extra high",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Section {
    #[default]
    Connection,
    Model,
    Limits,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Field {
    Name,
    Endpoint,
    Model,
    Credential,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Number {
    Selection,
    Timeout,
    OutputTokens,
}

#[derive(Clone)]
pub(crate) enum Message {
    Add,
    Wizard(wizard::Action),
    Select(u64),
    Section(Section),
    Edit(Field, String),
    Number(Number, String),
    Override(Number, bool),
    TrustLocal(bool),
    ModelMode(ModelMode),
    Vision(Vision),
    Reasoning(Reasoning),
    SetDefault,
    Delete,
    ConfirmDelete,
    CancelDelete,
    Test,
    Discover,
    Save,
    Refresh,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AI profile editor event")
    }
}

#[derive(Default)]
pub(super) struct Editor {
    catalog: Catalog,
    selected: Option<u64>,
    section: Section,
    invalid: BTreeMap<(u64, Number), String>,
    models: Vec<String>,
    status: Option<String>,
    deleting: bool,
    reserved_id: u64,
    wizard: Option<Wizard>,
}

impl Editor {
    pub(super) fn new(catalog: Catalog) -> Self {
        Self {
            selected: catalog.default,
            catalog,
            ..Self::default()
        }
    }

    pub(super) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub(super) fn draft_dirty(&self) -> bool {
        !self.invalid.is_empty() || self.wizard.is_some()
    }

    pub(super) fn validated_catalog(&self) -> Result<Catalog, String> {
        if self.wizard.is_some() {
            return Err("Finish or cancel the profile setup wizard first".into());
        }
        if !self.invalid.is_empty() {
            return Err("Correct the out-of-range numbers in the profile editor".into());
        }
        let mut catalog = self.catalog.clone();
        for profile in &mut catalog.profiles {
            profile.name = profile.name.trim().into();
            profile.endpoint = profile.endpoint.trim().into();
            profile.model = profile.model.trim().into();
            profile.credential_env = profile.credential_env.trim().into();
        }
        catalog.validate()?;
        Ok(catalog)
    }

    pub(super) fn replace(&mut self, catalog: Catalog) {
        let selection = self.selected;
        let section = self.section;
        let reserved_id = self.reserved_id;
        *self = Self::new(catalog);
        self.reserved_id = reserved_id;
        if selection.is_some_and(|id| self.catalog.profiles.iter().any(|profile| profile.id == id))
        {
            self.selected = selection;
        }
        self.section = section;
    }

    pub(super) fn selected(&self) -> Option<&Profile> {
        if let Some(wizard) = &self.wizard {
            return Some(&wizard.profile);
        }
        self.selected.and_then(|id| self.catalog.profile(id))
    }

    fn selected_mut(&mut self) -> Option<&mut Profile> {
        if let Some(wizard) = &mut self.wizard {
            return Some(&mut wizard.profile);
        }
        self.catalog
            .profiles
            .iter_mut()
            .find(|profile| Some(profile.id) == self.selected)
    }

    pub(super) fn set_models(&mut self, profile_id: u64, endpoint: &str, mut models: Vec<String>) {
        if self
            .selected()
            .is_some_and(|profile| profile.id == profile_id && profile.endpoint.trim() == endpoint)
        {
            models.retain(|model| {
                !model.is_empty()
                    && plain_text(model, local_ai::MAX_MODEL_BYTES)
                    && !model.chars().any(char::is_whitespace)
            });
            if let Some(profile) = self.selected_mut()
                && profile.model_mode == ModelMode::Auto
            {
                profile.model = models.first().cloned().unwrap_or_default();
            }
            models.sort_unstable();
            models.dedup();
            models.truncate(512);
            self.models = models;
        }
    }

    pub(super) fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    pub(super) fn update(&mut self, message: Message, editable: bool) {
        match message {
            Message::Select(id) if self.catalog.profiles.iter().any(|profile| profile.id == id) => {
                self.selected = Some(id);
                self.clear_selection_feedback();
                return;
            }
            Message::Section(section) => {
                self.section = section;
                return;
            }
            _ => {}
        }
        if !editable {
            return;
        }
        match message {
            Message::Add => {
                if let Some(profile) = self.new_profile() {
                    self.insert_profile(profile);
                }
            }
            Message::Wizard(action) => self.update_wizard(action),
            Message::Edit(field, value) => self.edit(field, value),
            Message::Number(field, input) => self.edit_number(field, input),
            Message::Override(field, enabled) => self.toggle_override(field, enabled),
            Message::TrustLocal(trusted) => {
                if let Some(profile) = self.selected_mut() {
                    profile.trusted_local_endpoint = trusted;
                    if profile.model_mode == ModelMode::Auto {
                        profile.model.clear();
                    }
                    if !trusted {
                        profile.credential_env.clear();
                    }
                }
                self.clear_selection_feedback();
            }
            Message::ModelMode(mode) => {
                if let Some(profile) = self.selected_mut()
                    && profile.model_mode != mode
                {
                    profile.model_mode = mode;
                    if mode == ModelMode::Auto {
                        profile.model.clear();
                    }
                    self.status = None;
                }
            }
            Message::Vision(vision) => {
                if let Some(profile) = self.selected_mut() {
                    profile.vision = vision;
                }
            }
            Message::Reasoning(reasoning) => {
                if let Some(profile) = self.selected_mut() {
                    profile.reasoning = reasoning;
                }
                self.status = None;
            }
            Message::SetDefault if self.selected.is_some() => self.catalog.default = self.selected,
            Message::Delete if self.selected.is_some() => self.deleting = true,
            Message::ConfirmDelete if self.deleting => self.delete(),
            Message::CancelDelete => self.deleting = false,
            _ => {}
        }
    }

    fn clear_selection_feedback(&mut self) {
        self.models.clear();
        self.status = None;
        self.deleting = false;
    }

    fn new_profile(&mut self) -> Option<Profile> {
        if self.catalog.profiles.len() >= MAX_PROFILES || self.wizard.is_some() {
            self.set_status("AI profile limit reached or setup already in progress");
            return None;
        }
        let Some(id) = self.catalog.last_id.max(self.reserved_id).checked_add(1) else {
            self.set_status("AI profile identifiers exhausted");
            return None;
        };
        let name = (1..=MAX_PROFILES)
            .map(|number| match number {
                1 => "New AI profile".into(),
                _ => format!("New AI profile {number}"),
            })
            .find(|name| {
                !self
                    .catalog
                    .profiles
                    .iter()
                    .any(|profile| profile.name.eq_ignore_ascii_case(name))
            })
            .unwrap_or_else(|| format!("AI profile {id}"));
        self.reserved_id = id;
        Some(Profile {
            id,
            name,
            ..Profile::default()
        })
    }

    fn insert_profile(&mut self, profile: Profile) {
        let id = profile.id;
        self.catalog.last_id = id;
        self.catalog.profiles.insert(0, profile);
        self.catalog.default.get_or_insert(id);
        self.selected = Some(id);
        self.section = Section::Connection;
        self.clear_selection_feedback();
    }

    fn delete(&mut self) {
        let Some(id) = self.selected else { return };
        self.catalog.profiles.retain(|profile| profile.id != id);
        self.invalid.retain(|(profile_id, _), _| *profile_id != id);
        self.selected = self.catalog.profiles.first().map(|profile| profile.id);
        if self.catalog.default == Some(id) {
            self.catalog.default = self.selected;
        }
        self.clear_selection_feedback();
    }

    fn edit(&mut self, field: Field, value: String) {
        let Some(profile) = self.selected_mut() else {
            return;
        };
        if (matches!(field, Field::Credential) && !profile.trusted_local_endpoint)
            || (matches!(field, Field::Model) && profile.model_mode == ModelMode::Auto)
        {
            return;
        }
        let (value, max, destination) = match field {
            Field::Name => (value, MAX_NAME_BYTES, &mut profile.name),
            Field::Endpoint => (value, MAX_ENDPOINT_BYTES, &mut profile.endpoint),
            Field::Model => (value, local_ai::MAX_MODEL_BYTES, &mut profile.model),
            Field::Credential => (value, 128, &mut profile.credential_env),
        };
        if value.len() <= max && value != *destination {
            *destination = value;
            if matches!(field, Field::Endpoint) {
                profile.trusted_local_endpoint = false;
                profile.credential_env.clear();
            }
            if matches!(field, Field::Endpoint | Field::Credential) {
                if profile.model_mode == ModelMode::Auto {
                    profile.model.clear();
                }
                self.models.clear();
            }
            self.status = None;
        }
    }

    fn edit_number(&mut self, field: Number, input: String) {
        let Some(id) = self.selected else { return };
        let parsed = input
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|value| match field {
                Number::Timeout => *value <= MAX_TIMEOUT_MINUTES,
                Number::Selection => (1..=MAX_SELECTION_CHARS).contains(value),
                Number::OutputTokens => (256..=1_048_576).contains(value),
            });
        let Some(value) = parsed else {
            self.invalid
                .insert((id, field), input.chars().take(32).collect());
            return;
        };
        if let Some(profile) = self.selected_mut() {
            match field {
                Number::Selection => profile.selection_limit = value,
                Number::Timeout if profile.timeout_minutes.is_some() => {
                    profile.timeout_minutes = Some(value)
                }
                Number::OutputTokens if profile.max_output_tokens.is_some() => {
                    profile.max_output_tokens = Some(value)
                }
                _ => return,
            }
        }
        self.invalid.remove(&(id, field));
        self.status = None;
    }

    fn toggle_override(&mut self, field: Number, enabled: bool) {
        let Some(profile) = self.selected_mut() else {
            return;
        };
        match field {
            Number::Timeout => profile.timeout_minutes = enabled.then_some(1),
            Number::OutputTokens => profile.max_output_tokens = enabled.then_some(16_000),
            _ => return,
        }
        let id = profile.id;
        self.invalid.remove(&(id, field));
        self.status = None;
    }
}

#[cfg(test)]
mod tests;
