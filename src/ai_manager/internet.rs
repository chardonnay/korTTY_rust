//! Saved General defaults for the existing reviewed MCP integrations.

use iced::widget::{button, column, row, text, text_input};
use iced::{Alignment, Element, Length};

use super::{Dialog, Message, Page, search::Setting};
use crate::local_ai::web_search::McpDefaults;

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

impl Dialog {
    pub(super) fn internet_view(&self) -> Element<'_, Message> {
        let enabled = self.editable() && self.profiles.web_search_allowed();
        let fields = Field::ALL.into_iter().fold(column![
            text("Select an Internet integration in Profiles to use these defaults. A blank profile plugin ID follows its global ID; an explicit profile ID keeps its own value. Each request retains its usual review.").size(13),
            text("Tavily and Bright Data labels apply to per-request LM Studio MCPs. Search credentials use their configured environment variables or LM Studio setup.").size(13),
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
}
