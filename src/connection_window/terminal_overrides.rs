use std::fmt;

use iced::widget::{
    Id, button, checkbox, column, container, keyed_column, pick_list, row, text, text_input,
};
use iced::{Element, Length, Task};
use kortty_terminal::TextEncoding;

use super::{HighlightChoices, journal_settings, view::ViewChoices};
use crate::ai_manager::{JavaProfileSource, ProfileRef};
use crate::connection_store::{ANSI_NAMES, TerminalOverrides, TerminalStyle, ThemeOverride};
use crate::terminal_agent::design::{Choice as AgentDesignChoice, valid_id as valid_design_id};
use crate::terminal_theme::{BoldTextMode, CursorStyle, Rgb, Scheme, Themes};
use crate::workspace::dialog_design::{self, keyboard_table};

mod skills;
pub(super) mod ssh_style;

#[derive(Clone, Debug)]
pub enum Edit {
    Show(bool),
    Reset,
    ResetTerminal,
    Theme(Option<ThemeOverride>),
    SshTheme(Option<ThemeOverride>, Option<Scheme>),
    FontFamily(String),
    FontSize(String),
    Color(usize, String),
    CursorStyle(Option<CursorStyle>),
    Bold(Option<BoldTextMode>),
    Colors(Option<bool>),
    Encoding(Option<TextEncoding>),
    Columns(String),
    Rows(String),
    ScrollbackLines(String),
    ScrollbackMemory(String),
    ScrollbackCompression(Option<bool>),
    ConfirmClose(Option<bool>),
    PasteLineDelay(String),
    StrictTerminalMode(Option<bool>),
    AiProfile(Option<ProfileRef>),
    AdoptJavaAiProfile(ProfileRef, JavaProfileSource),
    DismissJavaAiProfile,
    AiAgentDisabled(bool),
    AiAgentDesign(Option<String>),
    OpenMemory,
    AiSkillSearch(String),
    AiSkillNavigate(u64),
    AiSkill(u64, bool),
    HighlightSet(String),
    Journal(journal_settings::Edit),
}

impl Edit {
    pub(super) fn edits_style(&self) -> bool {
        matches!(
            self,
            Self::Theme(_)
                | Self::SshTheme(..)
                | Self::FontFamily(_)
                | Self::FontSize(_)
                | Self::Color(..)
                | Self::CursorStyle(_)
                | Self::Bold(_)
                | Self::Colors(_)
                | Self::ScrollbackLines(_)
                | Self::ScrollbackMemory(_)
                | Self::ScrollbackCompression(_)
                | Self::ConfirmClose(_)
        )
    }
}

pub(super) struct Draft {
    show: bool,
    options: TerminalOverrides,
    font: String,
    size: String,
    columns: String,
    rows: String,
    scrollback_lines: String,
    scrollback_memory: String,
    paste_line_delay: String,
    colors: [String; 20],
    journal: journal_settings::Draft,
    ai_profile_picker: Id,
    ai_design_picker: Id,
    ai_skill_list: Id,
    ai_skill_search: String,
    ai_skill_search_revision: u64,
    ai_skill_cursor: Option<u64>,
    style_theme_picker: Id,
    style_font_picker: Id,
    style_cursor_picker: Id,
    style_bold_picker: Id,
}

impl Draft {
    pub(super) fn new(options: &TerminalOverrides) -> Self {
        let mut colors = [
            options.foreground,
            options.background,
            options.cursor,
            options.selection,
        ]
        .into_iter()
        .chain(options.ansi);
        Self {
            show: !options.is_default(),
            options: options.clone(),
            journal: journal_settings::Draft::new(options.journal),
            ai_profile_picker: Id::unique(),
            ai_design_picker: Id::unique(),
            ai_skill_list: Id::unique(),
            ai_skill_search: String::new(),
            ai_skill_search_revision: 0,
            ai_skill_cursor: None,
            style_theme_picker: Id::unique(),
            style_font_picker: Id::unique(),
            style_cursor_picker: Id::unique(),
            style_bold_picker: Id::unique(),
            font: options.font_family.clone().unwrap_or_default(),
            size: options
                .font_size
                .map_or_else(String::new, |size| size.to_string()),
            columns: options
                .terminal_columns
                .map_or_else(String::new, |value| value.to_string()),
            rows: options
                .terminal_rows
                .map_or_else(String::new, |value| value.to_string()),
            scrollback_lines: options
                .scrollback_lines
                .map_or_else(String::new, |value| value.to_string()),
            scrollback_memory: options
                .scrollback_memory_mib
                .map_or_else(String::new, |value| value.to_string()),
            paste_line_delay: options
                .paste_line_delay_ms
                .map_or_else(String::new, |value| value.to_string()),
            colors: std::array::from_fn(|_| {
                colors
                    .next()
                    .flatten()
                    .map_or_else(String::new, |rgb| rgb.to_string())
            }),
        }
    }

    /// A visibility toggle leaves the edited values intact.
    pub(super) fn update(&mut self, edit: Edit) -> bool {
        match edit {
            Edit::Show(show) => {
                self.show = show;
                return false;
            }
            Edit::Reset => {
                self.options.dismiss_java_ai_profile();
                let dismissed = self.options.java_ai_profile_dismissed;
                *self = Self::new(&TerminalOverrides::default());
                self.options.java_ai_profile_dismissed = dismissed;
                self.show = true;
            }
            Edit::ResetTerminal => {
                let options = &self.options;
                let mut reset = Self::new(&TerminalOverrides {
                    ai_profile: options.ai_profile.clone(),
                    java_ai_profile: options.java_ai_profile.clone(),
                    java_ai_profile_dismissed: options.java_ai_profile_dismissed,
                    ai_agent_disabled: options.ai_agent_disabled,
                    ai_agent_design: options.ai_agent_design.clone(),
                    ai_skills: options.ai_skills.clone(),
                    ..TerminalOverrides::default()
                });
                std::mem::swap(&mut reset.journal, &mut self.journal);
                std::mem::swap(&mut reset.ai_skill_search, &mut self.ai_skill_search);
                reset.show = true;
                *self = reset;
            }
            Edit::Theme(value) => self.options.theme = value,
            Edit::SshTheme(value, scheme) => {
                self.options.theme = value;
                if let Some(scheme) = scheme {
                    self.font = scheme.family.label().to_owned();
                    self.size = scheme.font_size.to_string();
                    self.colors[0] = scheme.foreground.to_string();
                    self.colors[1] = scheme.background.to_string();
                }
            }
            Edit::FontFamily(value) => self.font = value.chars().take(160).collect(),
            Edit::FontSize(value) => self.size = value.chars().take(3).collect(),
            Edit::Color(index, value) => {
                let Some(field) = self.colors.get_mut(index) else {
                    return false;
                };
                let color = value.chars().take(7).collect();
                if *field == color {
                    return false;
                }
                *field = color;
            }
            Edit::CursorStyle(value) => self.options.cursor_style = value,
            Edit::Bold(value) => self.options.bold = value,
            Edit::Colors(value) => self.options.sgr_enabled = value,
            Edit::Encoding(value) => self.options.encoding = value,
            Edit::Columns(value) => self.columns = value.chars().take(7).collect(),
            Edit::Rows(value) => self.rows = value.chars().take(7).collect(),
            Edit::ScrollbackLines(value) => self.scrollback_lines = value.chars().take(7).collect(),
            Edit::ScrollbackMemory(value) => {
                self.scrollback_memory = value.chars().take(3).collect()
            }
            Edit::ScrollbackCompression(value) => self.options.scrollback_compression = value,
            Edit::ConfirmClose(value) => self.options.confirm_close = value,
            Edit::PasteLineDelay(value) => self.paste_line_delay = value.chars().take(8).collect(),
            Edit::StrictTerminalMode(value) => self.options.strict_terminal_mode = value,
            Edit::AiProfile(value) => {
                self.options.ai_profile = value;
                self.options.dismiss_java_ai_profile();
            }
            Edit::AdoptJavaAiProfile(reference, source) => {
                if self
                    .options
                    .java_ai_profile
                    .as_ref()
                    .and_then(|hint| hint.source())
                    != Some(&source)
                {
                    return false;
                }
                self.options.ai_profile = Some(reference);
                self.options.dismiss_java_ai_profile();
            }
            Edit::DismissJavaAiProfile => {
                if self.options.java_ai_profile.is_none() {
                    return false;
                }
                self.options.dismiss_java_ai_profile();
            }
            Edit::AiAgentDisabled(value) => self.options.ai_agent_disabled = value,
            Edit::AiAgentDesign(value) => {
                if !value.as_deref().is_none_or(valid_design_id) {
                    return false;
                }
                self.options.ai_agent_design = value;
            }
            Edit::OpenMemory => return false,
            Edit::AiSkillSearch(value) => {
                self.ai_skill_search = value.chars().take(160).collect();
                self.ai_skill_search_revision = self.ai_skill_search_revision.wrapping_add(1);
                self.ai_skill_cursor = None;
                return false;
            }
            Edit::AiSkillNavigate(id) => {
                self.ai_skill_cursor = Some(id);
                return false;
            }
            Edit::AiSkill(id, enabled) => {
                self.ai_skill_cursor = Some(id);
                self.options.ai_skills.retain(|value| *value != id);
                if enabled && self.options.ai_skills.len() < 32 {
                    self.options.ai_skills.push(id);
                }
            }
            Edit::HighlightSet(value) => {
                self.options.highlight_set =
                    (!value.trim().is_empty()).then(|| value.trim().to_owned())
            }
            Edit::Journal(edit) => self.journal.update(edit),
        }
        true
    }

    pub(super) fn value(&self) -> Result<TerminalOverrides, String> {
        self.value_with_style(true)
    }

    pub(super) fn seed_style(&mut self, style: &TerminalStyle) {
        style.write_terminal(&mut self.options);
        self.font = style.appearance.font_family.clone().unwrap_or_default();
        self.size = style
            .appearance
            .font_size
            .map_or_else(String::new, |size| size.to_string());
        self.scrollback_lines = style
            .scrollback_lines
            .map_or_else(String::new, |lines| lines.to_string());
        self.scrollback_memory = style
            .scrollback_memory_mib
            .map_or_else(String::new, |memory| memory.to_string());
        let colors = [
            style.appearance.foreground,
            style.appearance.background,
            style.cursor,
            style.selection,
        ]
        .into_iter()
        .chain(style.ansi);
        for (input, color) in self.colors.iter_mut().zip(colors) {
            *input = color.map_or_else(String::new, |rgb| rgb.to_string());
        }
    }

    pub(super) fn value_with_style(&self, own_style: bool) -> Result<TerminalOverrides, String> {
        let mut options = self.options.clone();
        options.paste_line_delay_ms = parse_paste_line_delay(&self.paste_line_delay)?;
        options.journal = self.journal.value()?;
        let columns = self.columns.trim();
        options.terminal_columns = (!columns.is_empty())
            .then(|| {
                columns
                    .parse()
                    .map_err(|_| "Enter 40 to 500 initial columns.")
            })
            .transpose()?;
        let rows = self.rows.trim();
        options.terminal_rows = (!rows.is_empty())
            .then(|| rows.parse().map_err(|_| "Enter 10 to 200 initial rows."))
            .transpose()?;
        if own_style {
            self.write_appearance_values(&mut options)?;
        } else {
            TerminalStyle::default().write_terminal(&mut options);
        }
        options.validate().map_err(|error| error.to_string())?;
        Ok(options)
    }

    fn write_appearance_values(&self, options: &mut TerminalOverrides) -> Result<(), String> {
        let font = self.font.trim();
        options.font_family = (!font.is_empty()).then(|| font.to_owned());
        let size = self.size.trim();
        options.font_size = (!size.is_empty())
            .then(|| {
                size.parse()
                    .map_err(|_| "Enter a terminal font size from 8 to 72.")
            })
            .transpose()?;
        let lines = self.scrollback_lines.trim();
        options.scrollback_lines = (!lines.is_empty())
            .then(|| {
                lines
                    .parse()
                    .map_err(|_| "Enter 100 to 100000 scrollback lines.")
            })
            .transpose()?;
        let memory = self.scrollback_memory.trim();
        options.scrollback_memory_mib = (!memory.is_empty())
            .then(|| {
                memory
                    .parse()
                    .map_err(|_| "Enter a scrollback memory limit from 1 to 50 MiB.")
            })
            .transpose()?;
        let mut colors = [None; 20];
        for (input, output) in self.colors.iter().zip(&mut colors) {
            if !input.trim().is_empty() {
                *output = Some(Rgb::parse(input).ok_or("Use #RRGGBB for terminal colors.")?);
            }
        }
        let [foreground, background, cursor, selection, ansi @ ..] = colors;
        options.foreground = foreground;
        options.background = background;
        options.cursor = cursor;
        options.selection = selection;
        options.ansi = ansi;
        Ok(())
    }

    pub(super) fn strict_mode_view(&self) -> Element<'_, Edit> {
        isolation_row(
            "Strict terminal mode",
            strict_terminal_mode(self.options.strict_terminal_mode, Edit::StrictTerminalMode),
        )
    }

    pub(super) fn ssh_highlight_picker(
        &self,
        catalog: &HighlightChoices<'_>,
        id: Id,
    ) -> Element<'static, Edit> {
        highlight_picker_with_keyboard(
            catalog,
            self.options.highlight_set.as_deref(),
            Edit::HighlightSet,
            Some(id),
        )
    }

    pub(super) fn ssh_paste_delay_view(&self) -> Element<'_, Edit> {
        labeled(
            "Paste line delay (ms)",
            text_input("Inherit", &self.paste_line_delay)
                .width(220)
                .on_input(Edit::PasteLineDelay)
                .style(dialog_design::form_input),
        )
    }

    pub(super) fn journal_view(&self, view: &ViewChoices<'_>) -> Element<'_, Edit> {
        self.journal.view(view.journal_policy).map(Edit::Journal)
    }

    pub(super) fn encoding_picker(&self, keyboard: Option<Id>) -> Element<'static, Edit> {
        let choices: Vec<_> = [Choice(None)]
            .into_iter()
            .chain(TextEncoding::ALL.map(|value| Choice(Some(value))))
            .collect();
        let picker = pick_list(choices, Some(Choice(self.options.encoding)), |choice| {
            Edit::Encoding(choice.0)
        })
        .width(Length::Fill);
        if let Some(id) = keyboard {
            picker.keyboard_navigation(id).into()
        } else {
            picker.into()
        }
    }

    pub(super) fn ai_view(
        &self,
        view: &ViewChoices<'_>,
        ssh_profile: Option<u64>,
    ) -> Element<'_, Edit> {
        let ssh = ssh_profile.is_some();
        let (mut ai_choices, mut selected) =
            ai_profile_choices(view.ai_profiles, self.options.ai_profile.as_ref());
        if ssh {
            ai_choices[0].1 = "Use default profile".into();
            ai_choices[1..].sort_by_cached_key(|item| item.1.to_lowercase());
            if selected.0.is_none() {
                selected = ai_choices[0].clone();
            } else if !ai_choices.contains(&selected) {
                ai_choices.push(selected.clone());
            }
        }
        let java_ai_profile = self.options.java_ai_profile.as_ref().map(|hint| {
            let target = hint.source().and_then(|source| {
                view.java_ai_profiles
                    .iter()
                    .find(|(candidate, _, _)| candidate == source)
            });
            let (label, adopt) = match target {
                Some((source, reference, name)) => (
                    format!("Imported Java AI profile · {name}"),
                    Some(Edit::AdoptJavaAiProfile(reference.clone(), source.clone())),
                ),
                None if hint.source().is_some() => (
                    format!(
                        "Imported Java AI profile · {} · Review the profile in AI Manager",
                        hint.id()
                    ),
                    None,
                ),
                None => (
                    format!(
                        "Imported Java AI profile · {} · Profile unavailable from this backup",
                        hint.id()
                    ),
                    None,
                ),
            };
            let use_profile = button("Use imported profile").on_press_maybe(adopt.clone());
            let dismiss = button("Dismiss").on_press(Edit::DismissJavaAiProfile);
            let (use_profile, dismiss) = if ssh {
                (
                    keyboard_table::action(use_profile.into(), adopt, true),
                    keyboard_table::action(dismiss.into(), Some(Edit::DismissJavaAiProfile), true),
                )
            } else {
                (use_profile.into(), dismiss.into())
            };
            row![text(label).width(Length::Fill), use_profile, dismiss].spacing(8)
        });
        let mut designs = view.ai_agent_designs.to_vec();
        let design_id = self.options.ai_agent_design.as_deref().unwrap_or_default();
        let selected_design = designs
            .iter()
            .find(|choice| choice.id == design_id)
            .cloned()
            .or_else(|| {
                if design_id.is_empty() {
                    return None;
                }
                let unavailable = AgentDesignChoice {
                    id: design_id.to_owned(),
                    name: "Stored design (currently unavailable)".into(),
                };
                designs.push(unavailable.clone());
                Some(unavailable)
            });
        let design: Element<'_, Edit> = if ssh && self.options.ai_agent_disabled {
            container(row![
                text(selected_design.map_or_else(|| "Use default".into(), |choice| choice.name))
                    .width(Length::Fill)
                    .size(13)
                    .style(text::secondary),
                text("▾").size(13).style(text::secondary),
            ])
            .padding(6)
            .width(Length::Fill)
            .style(container::bordered_box)
            .into()
        } else {
            let picker = pick_list(designs, selected_design, |choice| {
                Edit::AiAgentDesign((!choice.id.is_empty()).then_some(choice.id))
            })
            .width(Length::Fill)
            .text_size(13);
            if ssh {
                picker
                    .keyboard_navigation(self.ai_design_picker.clone())
                    .into()
            } else {
                picker.into()
            }
        };
        let profile = pick_list(ai_choices, Some(selected), |choice| {
            Edit::AiProfile(choice.0)
        })
        .width(Length::Fill);
        let agent_disabled = checkbox(self.options.ai_agent_disabled)
            .label("Disable terminal AI agent execution for this connection")
            .on_toggle(Edit::AiAgentDisabled);
        let choices = if let Some(id) = ssh_profile {
            let memory_action = (id != 0).then_some(Edit::OpenMemory);
            let memory = dialog_design::secondary_action("Edit connection memory…")
                .on_press_maybe(memory_action.clone());
            column![
                text("AI profile").size(13),
                profile.text_size(13).keyboard_navigation(self.ai_profile_picker.clone()),
                java_ai_profile,
                ai_hint("A stored profile assignment is kept if the profile becomes unavailable."),
                keyboard_table::action(
                    agent_disabled.text_size(13).into(),
                    Some(Edit::AiAgentDisabled(!self.options.ai_agent_disabled)),
                    false
                ),
                ai_hint("Ask and other AI chat features remain available when agent execution is disabled."),
                keyboard_table::action(memory.into(), memory_action, true),
                if id == 0 {
                    ai_hint("Save the connection to open its memory.")
                } else {
                    ai_hint("Connection memory provides rules for AI requests on this connection.")
                },
                text("AI agent design").size(13),
                design,
                ai_hint("Choose how agent runs appear in this connection."),
                text("Connection skills").size(13),
                ai_hint("Assign skills to this connection. Skills scoped to connections only apply where assigned."),
            ]
            .spacing(8)
        } else {
            column![
                labeled("AI chat profile", profile),
                java_ai_profile,
                agent_disabled,
                labeled("AI agent design", design),
                text("Assigned AI skills").size(14),
            ]
            .spacing(8)
        };
        self.ai_skill_list(choices, view.ai_skills, ssh)
    }

    fn ai_skill_list<'a>(
        &'a self,
        choices: iced::widget::Column<'a, Edit>,
        skills: &[crate::ai_manager::ConnectionSkillChoice<'_>],
        ssh: bool,
    ) -> Element<'a, Edit> {
        let skills = skills
            .iter()
            .filter(|choice| ssh || choice.available_to_quick_connect());
        if skills.clone().next().is_none() && self.options.ai_skills.is_empty() {
            return choices
                .push(text("No skills available").style(text::secondary))
                .into();
        }
        let query = self.ai_skill_search.trim().to_lowercase();
        let matches = |name: &str| name.to_lowercase().contains(&query);
        let choices = choices.push(
            text_input("Find skills", &self.ai_skill_search)
                .on_input(Edit::AiSkillSearch)
                .width(Length::Fill),
        );
        let mut visible = Vec::new();
        for choice in skills.clone().filter(|choice| {
            if ssh {
                choice.matches(&query)
            } else {
                matches(choice.name())
            }
        }) {
            let name = if ssh {
                choice.label()
            } else {
                choice.name().to_owned()
            };
            visible.push(skills::Row {
                id: choice.id,
                name,
                checked: self.options.ai_skills.contains(&choice.id),
            });
        }
        for &id in &self.options.ai_skills {
            if !skills.clone().any(|choice| choice.id == id) {
                let label = format!("Unavailable skill {id}");
                if matches(&label) {
                    visible.push(skills::Row {
                        id,
                        name: label,
                        checked: true,
                    });
                }
            }
        }
        if ssh {
            let list = skills::ssh(self.ai_skill_list.clone(), visible, self.ai_skill_cursor);
            choices
                .push(keyed_column![(self.ai_skill_search_revision, list)])
                .into()
        } else {
            choices.push(skills::local(visible)).into()
        }
    }

    pub(super) fn reveal_ai_skill<Message>(&self, id: u64) -> Task<Message> {
        skills::reveal(self.ai_skill_list.clone(), id)
    }

    pub(super) fn view(&self, view: &ViewChoices<'_>, combined: bool) -> Element<'_, Edit> {
        let heading = checkbox(self.show)
            .label("Show terminal overrides")
            .on_toggle(Edit::Show);
        if !self.show {
            return heading.into();
        }
        let highlighting = highlight_picker(
            &view.highlighting,
            self.options.highlight_set.as_deref(),
            Edit::HighlightSet,
        );
        let (themes, selected_theme) = theme_choices(view.themes, self.options.theme.as_ref());
        let mut choices = column![
            combined.then(
                || column![text("Session journal").size(16), self.journal_view(view)].spacing(8)
            ),
            labeled("Keyword highlighting", highlighting),
            labeled(
                "Theme",
                pick_list(themes, Some(selected_theme), |choice| Edit::Theme(
                    choice.value
                ))
                .width(Length::Fill)
            ),
            labeled(
                "Font family",
                text_input("Inherit", &self.font).on_input(Edit::FontFamily)
            ),
            labeled(
                "Font size",
                text_input("Inherit", &self.size).on_input(Edit::FontSize)
            ),
            labeled(
                "Cursor shape",
                pick(
                    CursorStyle::ALL,
                    self.options.cursor_style,
                    Edit::CursorStyle
                )
            ),
            labeled(
                "Bold text",
                pick(BoldTextMode::ALL, self.options.bold, Edit::Bold)
            ),
            labeled(
                "Terminal colors",
                toggle(self.options.sgr_enabled, Edit::Colors)
            ),
            combined.then(|| labeled("Text encoding", self.encoding_picker(None))),
            labeled(
                "Initial columns",
                text_input("Inherit", &self.columns).on_input(Edit::Columns)
            ),
            labeled(
                "Initial rows",
                text_input("Inherit", &self.rows).on_input(Edit::Rows)
            ),
            labeled(
                "Maximum scrollback lines",
                text_input("Inherit", &self.scrollback_lines).on_input(Edit::ScrollbackLines)
            ),
            labeled(
                "Scrollback memory limit (MiB)",
                text_input("Inherit", &self.scrollback_memory).on_input(Edit::ScrollbackMemory)
            ),
            labeled(
                "Scrollback compression",
                toggle(
                    self.options.scrollback_compression,
                    Edit::ScrollbackCompression
                )
            ),
            labeled(
                "Confirm closing",
                toggle(self.options.confirm_close, Edit::ConfirmClose)
            ),
            labeled(
                "Paste line delay (ms)",
                text_input("Inherit", &self.paste_line_delay).on_input(Edit::PasteLineDelay)
            ),
        ]
        .spacing(6);
        if combined {
            choices = choices.push(self.ai_view(view, None));
        }
        let labels = ["Foreground", "Background", "Cursor", "Selection"]
            .into_iter()
            .chain(ANSI_NAMES);
        let fields = labels
            .enumerate()
            .fold(column![].spacing(6), |fields, (index, label)| {
                fields.push(labeled(
                    label,
                    text_input("Inherit", &self.colors[index])
                        .on_input(move |value| Edit::Color(index, value)),
                ))
            });
        let reset = if combined {
            button("Use global defaults").on_press(Edit::Reset)
        } else {
            button("Use global terminal defaults").on_press(Edit::ResetTerminal)
        };
        let header = row![heading, reset]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        let hint = if combined {
            "Clear a field or select Inherit to follow the current default. Initial size applies when opening a terminal. Text encoding applies to new sessions and reconnects."
        } else {
            "Clear a field or select Inherit to follow the current default. Initial size applies when opening a terminal."
        };
        column![header, text(hint).size(12), choices, fields]
            .spacing(8)
            .into()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ThemeChoice {
    pub(crate) value: Option<ThemeOverride>,
    label: String,
}

impl fmt::Display for ThemeChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

pub(crate) fn theme_choices(
    themes: &Themes,
    stored: Option<&ThemeOverride>,
) -> (Vec<ThemeChoice>, ThemeChoice) {
    let inherit = ThemeChoice {
        value: None,
        label: "Inherit".into(),
    };
    let resolved = stored
        .and_then(|stored| stored.resolved_theme(themes))
        .map(|theme| theme.id);
    let mut choices = vec![inherit.clone()];
    choices.extend(themes.iter().map(|theme| ThemeChoice {
        value: if resolved == Some(theme.id) {
            stored.cloned()
        } else {
            Some(theme.id.into())
        },
        label: theme.name.into(),
    }));
    let selected = stored.map_or(inherit, |stored| {
        choices
            .iter()
            .find(|choice| choice.value.as_ref() == Some(stored))
            .cloned()
            .unwrap_or_else(|| {
                let label = match stored {
                    ThemeOverride::BuiltIn(preset) => format!("Unavailable theme ({preset})"),
                    ThemeOverride::Custom { custom } => {
                        format!("Unavailable custom theme ({custom})")
                    }
                    ThemeOverride::Java { java } => format!("Unavailable Java theme ({java})"),
                };
                let missing = ThemeChoice {
                    value: Some(stored.clone()),
                    label,
                };
                choices.push(missing.clone());
                missing
            })
    });
    (choices, selected)
}

#[derive(Clone, Eq, PartialEq)]
struct HighlightChoice {
    id: String,
    label: String,
}

pub(super) fn highlight_picker<'a, Message: Clone + 'a>(
    catalog: &HighlightChoices<'_>,
    stored: Option<&str>,
    on_change: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    highlight_picker_with_keyboard(catalog, stored, on_change, None)
}

fn highlight_picker_with_keyboard<'a, Message: Clone + 'a>(
    catalog: &HighlightChoices<'_>,
    stored: Option<&str>,
    on_change: impl Fn(String) -> Message + 'a,
    keyboard: Option<Id>,
) -> Element<'a, Message> {
    let (sets, selected) = highlight_choices(catalog, stored);
    let mut control = pick_list(sets, Some(selected), move |choice: HighlightChoice| {
        on_change(choice.id)
    })
    .width(Length::Fill);
    if let Some(id) = keyboard {
        control = control.keyboard_navigation(id);
    }
    let mut picker = column![control].spacing(4);
    if !catalog.enabled {
        picker = picker.push(text("Terminal highlighting is off globally.").size(12));
    }
    picker.into()
}

impl fmt::Display for HighlightChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn highlight_choices(
    catalog: &HighlightChoices<'_>,
    stored: Option<&str>,
) -> (Vec<HighlightChoice>, HighlightChoice) {
    let default_name = catalog
        .default_set
        .and_then(|default| catalog.sets.iter().find(|(id, _)| id == default))
        .map_or("None", |(_, name)| name.as_str());
    let inherit = HighlightChoice {
        id: String::new(),
        label: format!("Inherit ({default_name})"),
    };
    let mut choices = vec![
        inherit.clone(),
        HighlightChoice {
            id: "none".into(),
            label: "None".into(),
        },
    ];
    choices.extend(catalog.sets.iter().map(|(id, name)| HighlightChoice {
        id: id.clone(),
        label: name.clone(),
    }));
    let selected = stored.map_or(inherit, |stored| {
        choices
            .iter()
            .find(|choice| choice.id == stored)
            .cloned()
            .unwrap_or_else(|| {
                let missing = HighlightChoice {
                    id: stored.into(),
                    label: format!("Missing set ({stored})"),
                };
                choices.push(missing.clone());
                missing
            })
    });
    (choices, selected)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AiChoice(pub(crate) Option<ProfileRef>, String);

#[test]
fn imported_ai_profile_adoption_requires_matching_provenance_and_replaces_the_hint() {
    let source = |fingerprint: u8| -> JavaProfileSource {
        serde_json::from_value(serde_json::json!({
            "id": "java-chat",
            "fingerprint": vec![fingerprint; 32],
        }))
        .unwrap()
    };
    let mut hint = crate::ai_manager::JavaAiProfileHint::from_id("java-chat").unwrap();
    let exact = source(1);
    hint.associate_source(&[&exact]);
    let options = TerminalOverrides {
        java_ai_profile: Some(hint),
        ..TerminalOverrides::default()
    };
    let mut dismissed = Draft::new(&options);
    assert!(dismissed.update(Edit::DismissJavaAiProfile));
    let dismissed = dismissed.value().unwrap();
    assert!(dismissed.java_ai_profile.is_none() && dismissed.java_ai_profile_dismissed);
    let mut draft = Draft::new(&options);
    let reference = ProfileRef::Numeric(4);
    assert!(!draft.update(Edit::AdoptJavaAiProfile(reference.clone(), source(2))));
    assert!(draft.value().unwrap().ai_profile.is_none());
    assert!(draft.update(Edit::AdoptJavaAiProfile(reference.clone(), exact)));
    let saved = draft.value().unwrap();
    assert_eq!(saved.ai_profile, Some(reference));
    assert!(saved.java_ai_profile.is_none());
    assert!(saved.java_ai_profile_dismissed);
}

pub(crate) fn ai_profile_choices(
    profiles: &[(Option<u64>, ProfileRef, String)],
    saved: Option<&ProfileRef>,
) -> (Vec<AiChoice>, AiChoice) {
    let inherit = AiChoice(None, "Inherit".into());
    let mut choices = vec![inherit.clone()];
    let mut selected = saved.map_or(inherit, |reference| {
        AiChoice(Some(reference.clone()), "Unavailable profile".into())
    });
    for (legacy_alias, reference, name) in profiles {
        let choice = AiChoice(Some(reference.clone()), name.clone());
        if saved.is_some_and(|saved| {
            saved == reference
                || matches!(saved, ProfileRef::Numeric(previous) if Some(*previous) == *legacy_alias)
        }) {
            selected = choice.clone();
        }
        choices.push(choice);
    }
    (choices, selected)
}

#[cfg(test)]
#[test]
fn connection_ai_picker_preserves_an_unavailable_policy_reference() {
    use serde_json::json;

    let legacy_id = 1_u64 << 63;
    let policy = ProfileRef::Policy("policy-connection".into());
    let profiles = [(
        Some(legacy_id),
        policy.clone(),
        "Organization profile".into(),
    )];
    let (_, legacy_choice) = ai_profile_choices(&profiles, Some(&ProfileRef::Numeric(legacy_id)));
    assert_eq!(legacy_choice.0, Some(policy.clone()));

    for saved in [json!(legacy_id), json!("policy-connection")] {
        let options: TerminalOverrides =
            serde_json::from_value(json!({"ai_profile": saved})).unwrap();
        let mut draft = Draft::new(&options);
        let (_, unavailable) = ai_profile_choices(&[], options.ai_profile.as_ref());
        assert_eq!(unavailable.to_string(), "Unavailable profile");
        assert!(draft.update(Edit::FontSize("14".into())));
        assert_eq!(
            serde_json::to_value(draft.value().unwrap()).unwrap()["ai_profile"],
            saved
        );

        assert!(draft.update(Edit::AiProfile(legacy_choice.0.clone())));
        assert_eq!(draft.value().unwrap().ai_profile, Some(policy.clone()));
    }
}

#[cfg(test)]
#[test]
fn connection_ai_picker_does_not_infer_a_numeric_policy_alias() {
    let numeric = ProfileRef::Numeric(u64::MAX);
    let named = ProfileRef::Policy("policy-connection".into());
    let choices = [(None, named.clone(), "Organization profile".into())];
    let (_, unavailable) = ai_profile_choices(&choices, Some(&numeric));
    assert_eq!(unavailable.to_string(), "Unavailable profile");
    assert_eq!(unavailable.0, Some(numeric.clone()));
    let (_, selected) = ai_profile_choices(&choices, Some(&named));
    assert_eq!(selected.to_string(), "Organization profile");
    assert_eq!(selected.0, Some(named.clone()));

    let choices = [
        (None, numeric.clone(), "User profile".into()),
        (None, named, "Organization profile".into()),
    ];
    let (_, selected) = ai_profile_choices(&choices, Some(&numeric));
    assert_eq!(selected.to_string(), "User profile");
    assert_eq!(selected.0, Some(numeric));
}

impl fmt::Display for AiChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.1)
    }
}

pub(super) fn isolation_row<'a, Message: 'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).width(160), container(control).width(280)]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into()
}

fn labeled<'a>(label: &'static str, control: impl Into<Element<'a, Edit>>) -> Element<'a, Edit> {
    row![text(label).width(128), control.into()]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into()
}

fn ai_hint(label: &'static str) -> iced::widget::Text<'static> {
    text(label).size(13).style(text::secondary)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Choice<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Choice<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(value) => value.fmt(formatter),
            None => formatter.write_str("Inherit"),
        }
    }
}

fn pick<T: Copy + Eq + fmt::Display + 'static, const N: usize>(
    options: [T; N],
    selected: Option<T>,
    edit: fn(Option<T>) -> Edit,
) -> Element<'static, Edit> {
    let choices: Vec<_> = [Choice(None)]
        .into_iter()
        .chain(options.map(|value| Choice(Some(value))))
        .collect();
    pick_list(choices, Some(Choice(selected)), move |choice| {
        edit(choice.0)
    })
    .width(Length::Fill)
    .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Toggle(bool);

impl fmt::Display for Toggle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if self.0 { "On" } else { "Off" })
    }
}

fn toggle(selected: Option<bool>, edit: fn(Option<bool>) -> Edit) -> Element<'static, Edit> {
    let choices = [
        Choice(None),
        Choice(Some(Toggle(true))),
        Choice(Some(Toggle(false))),
    ];
    pick_list(choices, Some(Choice(selected.map(Toggle))), move |choice| {
        edit(choice.0.map(|value| value.0))
    })
    .width(Length::Fill)
    .into()
}

pub(super) fn parse_paste_line_delay(value: &str) -> Result<Option<u16>, String> {
    let value = value.trim();
    let delay = (!value.is_empty())
        .then(|| {
            value
                .parse()
                .map_err(|_| "Enter a paste line delay from 0 to 1000 ms.")
        })
        .transpose()?;
    crate::connection_store::validate_paste_line_delay(delay).map_err(|error| error.to_string())?;
    Ok(delay)
}

pub(super) fn strict_terminal_mode<'a, M: Clone + 'a>(
    selected: Option<bool>,
    edit: fn(Option<bool>) -> M,
) -> Element<'a, M> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    struct Strict(Option<bool>);
    impl fmt::Display for Strict {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(match self.0 {
                None => "Automatic (on in a sandbox)",
                Some(true) => "On",
                Some(false) => "Off",
            })
        }
    }
    let choices = [Strict(None), Strict(Some(true)), Strict(Some(false))];
    pick_list(choices, Some(Strict(selected)), move |choice| {
        edit(choice.0)
    })
    .width(Length::Fill)
    .into()
}

#[cfg(test)]
#[test]
fn connection_highlight_picker_preserves_inherit_none_and_missing_ids() {
    let catalog = HighlightChoices {
        sets: &[
            ("builtin.errors".into(), "Errors and warnings".into()),
            ("ops-errors".into(), "Operations errors".into()),
        ],
        default_set: Some("ops-errors"),
        enabled: false,
    };
    let (choices, selected) = highlight_choices(&catalog, Some("missing-shared-set"));
    assert_eq!(
        choices
            .iter()
            .map(|choice| (choice.id.as_str(), choice.label.as_str()))
            .collect::<Vec<_>>(),
        [
            ("", "Inherit (Operations errors)"),
            ("none", "None"),
            ("builtin.errors", "Errors and warnings"),
            ("ops-errors", "Operations errors"),
            ("missing-shared-set", "Missing set (missing-shared-set)"),
        ]
    );
    assert_eq!(selected.id, "missing-shared-set");
    for (stored, expected) in [
        (None, ""),
        (Some("none"), "none"),
        (Some("ops-errors"), "ops-errors"),
    ] {
        let (_, selected) = highlight_choices(&catalog, stored);
        assert_eq!(selected.id, expected);
    }
    let (_, selected) = highlight_choices(
        &HighlightChoices {
            default_set: Some("deleted-default"),
            ..catalog
        },
        None,
    );
    assert_eq!(selected.label, "Inherit (None)");
}

#[cfg(test)]
#[test]
fn connection_theme_picker_uses_custom_names_and_preserves_missing_ids() {
    use crate::terminal_theme::{Preset, Scheme};

    let mut themes = Themes::default();
    let mut draft = Draft::new(&TerminalOverrides::default());
    let id = themes.store(None, "Operations", Scheme::default()).unwrap();
    let custom = ThemeOverride::from(id);
    assert!(draft.update(Edit::Theme(Some(custom.clone()))));
    let saved = draft.value().unwrap();
    let (choices, selected) = theme_choices(&themes, saved.theme.as_ref());
    assert_eq!(choices.len(), 1 + Preset::ALL.len() + 1);
    assert_eq!(selected.value, Some(custom.clone()));
    assert_eq!(selected.label, "Operations");

    themes.remove(id);
    let mut reopened = Draft::new(&saved);
    let (choices, selected) = theme_choices(&themes, reopened.options.theme.as_ref());
    assert_eq!(choices.len(), 1 + Preset::ALL.len() + 1);
    assert_eq!(selected.value, Some(custom));
    assert!(selected.label.starts_with("Unavailable custom theme"));
    assert_eq!(reopened.value().unwrap(), saved);
    reopened.update(Edit::Theme(None));
    assert!(reopened.value().unwrap().theme.is_none());
}

#[cfg(test)]
#[test]
fn connection_theme_picker_displays_imported_name_without_rewriting_java_link() {
    use crate::terminal_theme::java;

    let scratch = crate::snippets::tests::Profile::new();
    let path = scratch.0.join("themes.xml");
    std::fs::write(
        &path,
        b"<themes><theme id=\"My-Java-Theme\"><name>Java Operations</name></theme></themes>",
    )
    .unwrap();
    let stored = ThemeOverride::from_java("My-Java-Theme".into()).unwrap();
    let mut themes = Themes::default();
    let (_, missing) = theme_choices(&themes, Some(&stored));
    assert!(missing.label.starts_with("Unavailable Java theme"));
    java::merge(&mut themes, java::read(&path).unwrap()).unwrap();
    let (choices, selected) = theme_choices(&themes, Some(&stored));
    assert_eq!(selected.label, "Java Operations");
    assert_eq!(selected.value, Some(stored));
    assert_eq!(
        choices
            .iter()
            .filter(|choice| choice.label == selected.label)
            .count(),
        1
    );
    assert!(choices.contains(&selected));
}
