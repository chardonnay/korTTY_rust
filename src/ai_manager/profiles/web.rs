//! Per-profile Internet integration settings.

use std::fmt;

use iced::Element;
use iced::widget::{column, text, text_input};

use crate::local_ai::web_search::{McpMode, McpSettings};

use super::{ConnectionMode, Editor, Message, Profile, view::choice};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Choice {
    Disabled,
    Tavily,
    Mcp(McpMode),
}

impl Choice {
    const STANDARD: [Self; 2] = [Self::Disabled, Self::Tavily];
    const LM_STUDIO: [Self; 7] = [
        Self::Disabled,
        Self::Tavily,
        Self::Mcp(McpMode::Tavily),
        Self::Mcp(McpMode::BrightData),
        Self::Mcp(McpMode::Brave),
        Self::Mcp(McpMode::Searxng),
        Self::Mcp(McpMode::Toolpack),
    ];
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Tavily => formatter.write_str("Tavily · direct search"),
            Self::Mcp(mode) => mode.fmt(formatter),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Action {
    Select(Choice),
    Plugin(String),
    Tools(String),
}

impl Profile {
    pub(in crate::ai_manager) fn has_web_search(&self) -> bool {
        self.tavily_search || self.lm_web.is_some()
    }

    pub(super) fn clear_web_search(&mut self) {
        self.tavily_search = false;
        self.lm_web = None;
    }

    fn web_choice(&self) -> Choice {
        if let Some(settings) = &self.lm_web {
            Choice::Mcp(settings.mode)
        } else if self.tavily_search {
            Choice::Tavily
        } else {
            Choice::Disabled
        }
    }
}

impl Editor {
    pub(super) fn update_web_search(&mut self, action: Action) {
        let Some(profile) = self.selected_mut().filter(|profile| !profile.is_cli()) else {
            return;
        };
        match action {
            Action::Select(mode) if mode != profile.web_choice() => match mode {
                Choice::Disabled => profile.clear_web_search(),
                Choice::Tavily => {
                    profile.clear_web_search();
                    profile.tavily_search = true;
                }
                Choice::Mcp(mode) if profile.connection_mode == ConnectionMode::LmStudio => {
                    profile.clear_web_search();
                    profile.lm_web = Some(McpSettings::new(mode));
                }
                _ => return,
            },
            Action::Plugin(plugin)
                if plugin.len() <= 68 && !plugin.chars().any(char::is_control) =>
            {
                if let Some(settings) = &mut profile.lm_web
                    && settings.mode.uses_plugin()
                {
                    settings.plugin = plugin;
                }
            }
            Action::Tools(tools) if tools.len() <= 520 && !tools.chars().any(char::is_control) => {
                if let Some(settings) = &mut profile.lm_web
                    && settings.mode == McpMode::Toolpack
                {
                    settings.tool_names = tools;
                }
            }
            _ => return,
        }
        self.status = None;
    }

    pub(super) fn web_search_view<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
    ) -> Element<'a, Message> {
        if self.web_search_denied {
            return text("Managed policy blocks web search.").size(13).into();
        }
        let mode = profile.web_choice();
        let select = |mode| Message::Web(Action::Select(mode));
        let picker = if profile.connection_mode == ConnectionMode::LmStudio {
            choice(Choice::LM_STUDIO, mode, editable, select)
        } else {
            choice(Choice::STANDARD, mode, editable, select)
        };
        let mut fields = column![text("Terminal Chat web search"), picker].spacing(6);
        match mode {
            Choice::Disabled => {}
            Choice::Tavily => {
                fields = fields.push(text("Direct search sends the separately reviewed query to api.tavily.com using TAVILY_API_KEY.").size(13));
            }
            Choice::Mcp(mode) => {
                let settings = profile.lm_web.as_ref().expect("selected MCP mode");
                fields = fields.push(text(mode.setup()).size(13));
                if mode.uses_plugin() {
                    fields = fields.push(
                        text_input("Installed plugin ID (mcp/server-name)", &settings.plugin)
                            .on_input_maybe(
                                editable.then_some(|id| Message::Web(Action::Plugin(id))),
                            ),
                    );
                }
                if mode == McpMode::Toolpack {
                    fields = fields.push(
                        text_input(
                            "Allowed Toolpack names, separated by spaces",
                            &settings.tool_names,
                        )
                        .on_input_maybe(
                            editable.then_some(|tools| Message::Web(Action::Tools(tools))),
                        ),
                    );
                }
            }
        }
        fields.into()
    }
}
