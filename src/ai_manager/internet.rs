//! Saved General defaults for the existing reviewed MCP integrations.

use iced::widget::{button, checkbox, column, row, text, text_input};
use iced::{Alignment, Element, Length};
use kortty_vault::{WebCredentialChange, WebProvider};

use super::{Dialog, Message, Page, search::Setting};
use crate::local_ai::web_search::McpDefaults;
use crate::vault_window::{SecretEdit, VaultWindow};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Field {
    SearxngUrl,
    TavilyLabel,
    BrightDataLabel,
    BravePlugin,
    SearxngPlugin,
    ToolpackPlugin,
}

impl Field {
    pub(super) const ALL: [Self; 6] = [
        Self::SearxngUrl,
        Self::TavilyLabel,
        Self::BrightDataLabel,
        Self::BravePlugin,
        Self::SearxngPlugin,
        Self::ToolpackPlugin,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::SearxngUrl => "Global SearXNG URL",
            Self::TavilyLabel => "Tavily MCP server label",
            Self::BrightDataLabel => "Bright Data MCP server label",
            Self::BravePlugin => "Global Brave Search MCP plugin ID",
            Self::SearxngPlugin => "Global SearXNG MCP plugin ID",
            Self::ToolpackPlugin => "Global Toolpack MCP plugin ID",
        }
    }

    pub(super) fn keywords(self) -> &'static str {
        match self {
            Self::SearxngUrl => {
                "internet access global SearXNG base URL required configuration LM Studio server setup"
            }
            Self::TavilyLabel | Self::BrightDataLabel => {
                "LM Studio internet access global default MCP server label per-request integration"
            }
            _ => {
                "LM Studio internet access global default MCP plugin identifier inherit profile integration"
            }
        }
    }

    fn value(self, settings: &McpDefaults) -> &str {
        match self {
            Self::SearxngUrl => &settings.searxng_url,
            Self::TavilyLabel => &settings.tavily_label,
            Self::BrightDataLabel => &settings.bright_data_label,
            Self::BravePlugin => &settings.brave_plugin,
            Self::SearxngPlugin => &settings.searxng_plugin,
            Self::ToolpackPlugin => &settings.toolpack_plugin,
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::SearxngUrl => "https://search.example.com/",
            Self::TavilyLabel => "tavily",
            Self::BrightDataLabel => "bright-data",
            _ => "mcp/server-name",
        }
    }

    pub(super) fn set(self, settings: &mut McpDefaults, value: String) {
        let limit = crate::local_ai::web_search::MAX_DEFAULT_BYTES;
        if value.len() > limit || value.chars().any(char::is_control) {
            return;
        }
        *match self {
            Self::SearxngUrl => &mut settings.searxng_url,
            Self::TavilyLabel => &mut settings.tavily_label,
            Self::BrightDataLabel => &mut settings.bright_data_label,
            Self::BravePlugin => &mut settings.brave_plugin,
            Self::SearxngPlugin => &mut settings.searxng_plugin,
            Self::ToolpackPlugin => &mut settings.toolpack_plugin,
        } = value;
    }
}

#[derive(Default)]
struct CredentialEdit {
    value: Option<SecretEdit>,
    clear: bool,
}

#[derive(Default)]
pub(super) struct CredentialsDraft {
    edits: [CredentialEdit; 3],
    revision: Option<[u8; 32]>,
}

fn index(provider: WebProvider) -> usize {
    match provider {
        WebProvider::Tavily => 0,
        WebProvider::BrightData => 1,
        WebProvider::Brave => 2,
    }
}

pub(super) fn clear_label(provider: WebProvider) -> &'static str {
    match provider {
        WebProvider::Tavily => "Clear Tavily API key",
        WebProvider::BrightData => "Clear Bright Data API token",
        WebProvider::Brave => "Clear Brave Search API key",
    }
}

impl CredentialsDraft {
    pub(super) fn dirty(&self) -> bool {
        self.edits
            .iter()
            .any(|edit| edit.value.is_some() || edit.clear)
    }

    pub(super) fn set(&mut self, provider: WebProvider, value: SecretEdit) {
        let edit = &mut self.edits[index(provider)];
        if !edit.clear && value.value().len() <= kortty_vault::MAX_WEB_CREDENTIAL_BYTES {
            edit.value = (!value.value().is_empty()).then_some(value);
        }
        if !self.dirty() {
            self.revision = None;
        }
    }

    pub(super) fn clear(&mut self, provider: WebProvider, clear: bool) {
        let edit = &mut self.edits[index(provider)];
        edit.clear = clear;
        if clear {
            edit.value = None;
        }
        if !self.dirty() {
            self.revision = None;
        }
    }
}

impl Dialog {
    fn editable_web_revision(&self, vault: Option<&VaultWindow>) -> Option<[u8; 32]> {
        if !self.editable() || !self.profiles.web_search_allowed() {
            return None;
        }
        let vault = vault.filter(|vault| vault.can_manage_environments())?;
        let revision = vault.web_credential_revision()?;
        self.web_credentials
            .revision
            .is_none_or(|past| past == revision)
            .then_some(revision)
    }

    pub(super) fn begin_web_credential_edit(&mut self, vault: Option<&VaultWindow>) -> bool {
        if crate::local_ai::web_search::require_policy().is_err() {
            self.profiles.deny_web_search(true);
            return false;
        }
        if let Some(revision) = self.editable_web_revision(vault) {
            self.web_credentials.revision = Some(revision);
            true
        } else {
            false
        }
    }

    pub(crate) fn save_web_credentials(&mut self, vault: Option<&mut VaultWindow>) -> bool {
        if !self.web_credentials.dirty() {
            return false;
        }
        if let Err(error) = crate::local_ai::web_search::require_policy() {
            self.profiles.deny_web_search(true);
            self.notice = error;
            return false;
        }
        let Some(revision) = self.editable_web_revision(vault.as_deref()) else {
            self.notice = "Unlock the vault or discard the stale credential edits.".into();
            return false;
        };
        let changes = WebProvider::ALL
            .into_iter()
            .filter_map(|provider| {
                let edit = &self.web_credentials.edits[index(provider)];
                let change = if edit.clear {
                    WebCredentialChange::Clear
                } else {
                    WebCredentialChange::Set(edit.value.clone()?.into_secret())
                };
                Some((provider, change))
            })
            .collect();
        let vault = vault.expect("editable vault");
        let result = vault.update_web_credentials(revision, changes);
        let changed = vault.web_credential_revision() != Some(revision);
        if changed || result.is_ok() {
            self.web_credentials = CredentialsDraft::default();
        }
        if result.is_err() && crate::local_ai::web_search::require_policy().is_err() {
            self.profiles.deny_web_search(true);
        }
        self.notice = match result {
            Ok(()) => "Internet credentials saved in the vault.".into(),
            Err(error) => format!("Could not save Internet credentials ({error})."),
        };
        changed
    }

    pub(super) fn internet_view(&self, vault: Option<&VaultWindow>) -> Element<'_, Message> {
        let enabled = self.editable() && self.profiles.web_search_allowed();
        let setting = Setting::AgentAlwaysOfferWebTools;
        let always = self.general.agent_always_offer_web_tools.unwrap_or(false);
        let offer_tools = self.revealed.keyboard_action(
            setting,
            checkbox(always)
                .label(setting.title())
                .on_toggle_maybe(enabled.then_some(Message::AgentAlwaysOfferWebTools)),
            enabled.then_some(Message::AgentAlwaysOfferWebTools(!always)),
            false,
        );
        let fields = Field::ALL.into_iter().fold(column![
            self.revealed.wrap(setting, offer_tools),
            text("Agents with direct Tavily can propose web research even when the task does not mention it. Each request and its returned excerpts still require approval.").size(13),
            text("Select an Internet integration in Profiles to use these defaults. A blank profile plugin ID follows its global ID; an explicit profile ID keeps its own value. Each request retains its usual review.").size(13),
            text("Tavily and Bright Data labels apply to per-request LM Studio MCPs. Brave Search uses its installed LM Studio plugin.").size(13),
            text("SearXNG requires its global URL and a plugin ID. Configure the matching server in LM Studio. This URL records the server setup; page reads use the separate profile MCP endpoint.").size(13),
        ].spacing(12), |fields, field| {
            let setting = Setting::Internet(field);
            let input = text_input(field.placeholder(), field.value(&self.general.internet))
                .on_input_maybe(enabled.then_some(move |value| Message::Internet(field, value)))
                .width(Length::Fill);
            fields.push(self.revealed.wrap(setting, row![
                text(field.title()).size(13).width(212),
                self.revealed.input(setting, enabled, input),
            ].spacing(12).align_y(Alignment::Center)))
        });
        fields
            .push(self.web_credential_view(vault))
            .push(
                self.general
                    .internet
                    .validate()
                    .err()
                    .map(|error| text(error).size(12)),
            )
            .push((!self.profiles.web_search_allowed()).then(|| {
                text("Managed policy blocks Internet access. These defaults are read-only.")
                    .size(13)
            }))
            .push(
                button("Open Profiles")
                    .style(button::secondary)
                    .on_press(Message::Page(Page::Profiles)),
            )
            .into()
    }

    fn web_credential_view(&self, vault: Option<&VaultWindow>) -> Element<'_, Message> {
        let enabled = self.editable_web_revision(vault).is_some();
        let mut fields = column![text("Provider credentials").size(16)].spacing(8);
        for provider in WebProvider::ALL {
            let field = Setting::WebCredential(provider);
            let clear = Setting::ClearWebCredential(provider);
            let edit = &self.web_credentials.edits[index(provider)];
            let stored = vault.is_some_and(|vault| vault.web_credential(provider).is_some());
            let placeholder = if stored {
                "Saved in vault"
            } else {
                "Enter new credential"
            };
            let input_enabled = enabled && !edit.clear;
            let value = if enabled {
                edit.value.as_ref().map_or("", SecretEdit::value)
            } else {
                ""
            };
            let input = text_input(placeholder, value).secure(true).on_input_maybe(
                input_enabled.then_some(move |value| {
                    Message::WebCredential(provider, SecretEdit::new(value))
                }),
            );
            let can_clear = enabled && (stored || edit.value.is_some() || edit.clear);
            let toggle = self.revealed.keyboard_action(
                clear,
                checkbox(edit.clear)
                    .label(clear_label(provider))
                    .on_toggle_maybe(
                        can_clear
                            .then_some(move |clear| Message::ClearWebCredential(provider, clear)),
                    ),
                can_clear.then_some(Message::ClearWebCredential(provider, !edit.clear)),
                false,
            );
            fields = fields
                .push(
                    self.revealed.wrap(
                        field,
                        row![
                            text(provider.label()).size(13).width(212),
                            self.revealed.input(field, input_enabled, input),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    ),
                )
                .push(self.revealed.wrap(clear, toggle));
        }
        let dirty = self.web_credentials.dirty();
        fields
            .push(text("Save credentials writes these edits to the encrypted vault. Blank fields keep saved values unless Clear is selected.").size(13))
            .push((!enabled && self.profiles.web_search_allowed()).then(|| {
                text(if self.web_credentials.revision.is_some() {
                    "Reopen or unlock the vault. If its contents changed, discard these edits and enter them again."
                } else {
                    "Unlock the vault to manage credentials."
                }).size(13)
            }))
            .push(row![
                button("Save credentials").on_press_maybe((enabled && dirty).then_some(Message::SaveWebCredentials)),
                button("Discard credential edits").on_press_maybe((self.editable() && dirty).then_some(Message::DiscardWebCredentials)),
                (!enabled && self.profiles.web_search_allowed()).then(|| button("Open Vault…").on_press(Message::OpenCredentialVault)),
            ].spacing(8).wrap())
            .into()
    }
}
