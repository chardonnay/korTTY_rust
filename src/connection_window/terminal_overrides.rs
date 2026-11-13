use std::fmt;

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::{Element, Length};
use kortty_terminal::TextEncoding;

use super::{HighlightChoices, journal_settings, view::ViewChoices};
use crate::ai_manager::ProfileRef;
use crate::connection_store::{ANSI_NAMES, TerminalOverrides, ThemeOverride};
use crate::terminal_agent::design::{Choice as AgentDesignChoice, valid_id as valid_design_id};
use crate::terminal_theme::{BoldTextMode, CursorStyle, Rgb, Themes};

#[derive(Clone, Debug)]
pub enum Edit {
    Show(bool),
    Reset,
    Theme(Option<ThemeOverride>),
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
    AiAgentDisabled(bool),
    AiAgentDesign(Option<String>),
    AiSkill(u64, bool),
    HighlightSet(String),
    Journal(journal_settings::Edit),
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
                *self = Self::new(&TerminalOverrides::default());
                self.show = true;
            }
            Edit::Theme(value) => self.options.theme = value,
            Edit::FontFamily(value) => self.font = value.chars().take(160).collect(),
            Edit::FontSize(value) => self.size = value.chars().take(3).collect(),
            Edit::Color(index, value) => {
                let Some(field) = self.colors.get_mut(index) else {
                    return false;
                };
                *field = value.chars().take(7).collect();
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
            Edit::AiProfile(value) => self.options.ai_profile = value,
            Edit::AiAgentDisabled(value) => self.options.ai_agent_disabled = value,
            Edit::AiAgentDesign(value) => {
                if !value.as_deref().is_none_or(valid_design_id) {
                    return false;
                }
                self.options.ai_agent_design = value;
            }
            Edit::AiSkill(id, enabled) => {
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
        let mut options = self.options.clone();
        options.paste_line_delay_ms = parse_paste_line_delay(&self.paste_line_delay)?;
        options.journal = self.journal.value()?;
        let font = self.font.trim();
        options.font_family = (!font.is_empty()).then(|| font.to_owned());
        options.font_size = if self.size.trim().is_empty() {
            None
        } else {
            Some(
                self.size
                    .trim()
                    .parse()
                    .map_err(|_| "Enter a terminal font size from 8 to 72.")?,
            )
        };
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
        options.validate().map_err(|error| error.to_string())?;
        Ok(options)
    }

    pub(super) fn view(&self, view: &ViewChoices<'_>) -> Element<'_, Edit> {
        let skills = view.ai_skills;
        let heading = checkbox(self.show)
            .label("Show terminal overrides")
            .on_toggle(Edit::Show);
        if !self.show {
            return heading.into();
        }
        let (ai_choices, selected) =
            ai_profile_choices(view.ai_profiles, self.options.ai_profile.as_ref());
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
        let (sets, selected_set) =
            highlight_choices(&view.highlighting, self.options.highlight_set.as_deref());
        let (themes, selected_theme) = theme_choices(view.themes, self.options.theme.as_ref());
        let mut highlighting = column![
            pick_list(sets, Some(selected_set), |choice: HighlightChoice| {
                Edit::HighlightSet(choice.id)
            })
            .width(Length::Fill)
        ]
        .spacing(4);
        if !view.highlighting.enabled {
            highlighting =
                highlighting.push(text("Terminal highlighting is off globally.").size(12));
        }
        let mut choices = column![
            self.journal.view(view.journal_max_parts).map(Edit::Journal),
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
            labeled(
                "Text encoding",
                pick(TextEncoding::ALL, self.options.encoding, Edit::Encoding)
            ),
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
            labeled(
                "Strict terminal mode",
                strict_terminal_mode(self.options.strict_terminal_mode, Edit::StrictTerminalMode)
            ),
            labeled(
                "AI chat profile",
                pick_list(ai_choices, Some(selected), |choice| Edit::AiProfile(
                    choice.0
                ))
                .width(Length::Fill)
            ),
            checkbox(self.options.ai_agent_disabled)
                .label("Disable terminal AI agent execution for this connection")
                .on_toggle(Edit::AiAgentDisabled),
            labeled(
                "AI agent design",
                pick_list(designs, selected_design, |choice| {
                    Edit::AiAgentDesign((!choice.id.is_empty()).then_some(choice.id))
                })
                .width(Length::Fill)
            ),
        ]
        .spacing(6);
        if !skills.is_empty() || !self.options.ai_skills.is_empty() {
            choices = choices.push(text("Assigned AI skills").size(14));
            for (id, name) in skills {
                let id = *id;
                choices = choices.push(
                    checkbox(self.options.ai_skills.contains(&id))
                        .label(name.clone())
                        .on_toggle(move |enabled| Edit::AiSkill(id, enabled)),
                );
            }
            for &id in &self.options.ai_skills {
                if !skills.iter().any(|(available, _)| *available == id) {
                    choices = choices.push(
                        checkbox(true)
                            .label(format!("Unavailable skill {id}"))
                            .on_toggle(move |enabled| Edit::AiSkill(id, enabled)),
                    );
                }
            }
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
        let header = row![heading, button("Use global defaults").on_press(Edit::Reset)]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        column![header, text("Clear a field or select Inherit to follow the current default. Initial size applies when opening a terminal. Text encoding applies to new sessions and reconnects.").size(12), choices, fields]
            .spacing(8)
            .into()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct ThemeChoice {
    value: Option<ThemeOverride>,
    label: String,
}

impl fmt::Display for ThemeChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn theme_choices(
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
struct AiChoice(Option<ProfileRef>, String);

fn ai_profile_choices(
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

fn labeled<'a>(label: &'static str, control: impl Into<Element<'a, Edit>>) -> Element<'a, Edit> {
    row![text(label).width(128), control.into()]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into()
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
