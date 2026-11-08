use std::fmt;

use iced::widget::{
    button, checkbox, column, container, pane_grid, pick_list, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Alignment, Element, Length};

use crate::session_journal::entry_edit::LegacyMarker;
use crate::session_journal::markers::{
    Definition, MAX_DEFINITIONS, MAX_NAME_BYTES, MAX_PATTERN_BYTES, MAX_RULES, Rule, Settings,
};
use crate::terminal_theme::Rgb;
use crate::workspace::chrome::popup;

use super::{Event, Operation, Phase};

const PALETTE: [u32; 7] = [
    0x7c3aed, 0x0e7490, 0xb45309, 0xbe123c, 0x15803d, 0x4338ca, 0xa21caf,
];
const NEW_MARKER: &str = "New marker";

#[derive(Clone)]
pub(crate) enum Edit {
    Add,
    Duplicate,
    Delete,
    SelectDefinition(String),
    Name(String, String),
    ShowColor(String),
    Color(String, String),
    CloseColor,
    AddRule,
    RemoveRule,
    MoveRule(bool),
    SelectRule(u64, usize),
    ChangeRule(u64, usize, RuleChange),
    Enabled(bool),
    OverwriteManual(bool),
    Resize(pane_grid::ResizeEvent),
}

impl fmt::Debug for Edit {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("JournalMarkerEdit")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) enum RuleChange {
    Enabled(bool),
    Marker(String),
    Pattern(String),
    Regex(bool),
    IgnoreCase(bool),
}

#[derive(Clone, Copy)]
enum Section {
    Definitions,
    Rules,
}

struct ColorEdit {
    id: String,
    text: String,
}

pub(super) struct Form {
    settings: Settings,
    definition: Option<String>,
    selected_rule: Option<usize>,
    rule_revision: u64,
    color: Option<ColorEdit>,
    status: Option<String>,
    overwrite_manual: bool,
    panes: pane_grid::State<Section>,
}

impl Form {
    pub(super) fn new(settings: Settings) -> Self {
        let (mut panes, definitions) = pane_grid::State::new(Section::Definitions);
        let _ = panes.split(pane_grid::Axis::Horizontal, definitions, Section::Rules);
        Self {
            settings,
            definition: None,
            selected_rule: None,
            rule_revision: 0,
            color: None,
            status: None,
            overwrite_manual: false,
            panes,
        }
    }

    pub(super) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(super) fn overwrite_manual(&self) -> bool {
        self.overwrite_manual
    }

    pub(super) fn set_status(&mut self, status: Option<String>) {
        self.color = None;
        self.status = status;
    }

    fn select_definition(&mut self, id: String) {
        if self
            .settings
            .definitions
            .iter()
            .any(|definition| definition.id == id)
        {
            self.definition = Some(id);
        }
    }

    fn definition_mut(&mut self, id: &str) -> Option<&mut Definition> {
        self.settings
            .definitions
            .iter_mut()
            .find(|definition| definition.id == id)
    }

    pub(super) fn update(&mut self, event: Edit) {
        match event {
            Edit::Resize(event) => {
                self.panes
                    .resize(event.split, event.ratio.clamp(0.26, 0.70));
            }
            Edit::Add => {
                let color = Rgb::from_hex(PALETTE[self.settings.definitions.len() % PALETTE.len()]);
                match self
                    .settings
                    .add_definition(NEW_MARKER, color.bytes(), LegacyMarker::Info)
                {
                    Ok(index) => {
                        self.definition = Some(self.settings.definitions[index].id.clone())
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            Edit::Duplicate => {
                if let Some(id) = &self.definition
                    && let Err(error) = self.settings.duplicate_definition(id)
                {
                    self.status = Some(error.to_string());
                }
            }
            Edit::Delete => {
                if let Some(id) = self.definition.take() {
                    self.settings
                        .definitions
                        .retain(|definition| definition.id != id);
                    self.settings.rules.retain(|rule| rule.marker_id != id);
                    self.color = None;
                    self.selected_rule = None;
                    self.rules_changed();
                }
            }
            Edit::SelectDefinition(id) => self.select_definition(id),
            Edit::Name(id, name) if name.len() <= MAX_NAME_BYTES => {
                if let Some(definition) = self.definition_mut(&id) {
                    definition.name = name;
                    self.definition = Some(id);
                }
            }
            Edit::ShowColor(id) => {
                if let Some(definition) = self.definition_mut(&id) {
                    let text = definition
                        .color
                        .map(Rgb::from_bytes)
                        .map_or_else(String::new, |color| color.to_string());
                    self.color = Some(ColorEdit {
                        id: id.clone(),
                        text,
                    });
                    self.definition = Some(id);
                }
            }
            Edit::Color(id, value) if value.len() <= 7 => {
                if let Some(editor) = self.color.as_mut().filter(|editor| editor.id == id) {
                    let parsed = Rgb::parse(&value);
                    editor.text = value;
                    if let Some(color) = parsed
                        && let Some(definition) = self.definition_mut(&id)
                    {
                        definition.color = Some(color.bytes());
                    }
                }
            }
            Edit::CloseColor => self.color = None,
            Edit::AddRule if self.settings.rules.len() < MAX_RULES => {
                if let Some(definition) = self.settings.definitions.first() {
                    self.selected_rule = Some(self.settings.rules.len());
                    self.settings.rules.push(Rule {
                        marker_id: definition.id.clone(),
                        ..Rule::default()
                    });
                    self.rules_changed();
                }
            }
            Edit::RemoveRule => {
                if let Some(index) = self
                    .selected_rule
                    .take()
                    .filter(|index| *index < self.settings.rules.len())
                {
                    self.settings.rules.remove(index);
                    self.selected_rule = (!self.settings.rules.is_empty())
                        .then_some(index.min(self.settings.rules.len().saturating_sub(1)));
                    self.rules_changed();
                }
            }
            Edit::MoveRule(down) => {
                if let Some(index) = self.selected_rule
                    && let Some(target) = self.rule_destination(down)
                {
                    self.settings.rules.swap(index, target);
                    self.selected_rule = Some(target);
                    self.rules_changed();
                }
            }
            Edit::SelectRule(revision, index)
                if revision == self.rule_revision && index < self.settings.rules.len() =>
            {
                self.selected_rule = Some(index);
            }
            Edit::ChangeRule(revision, index, change) if revision == self.rule_revision => {
                self.change_rule(index, change);
            }
            Edit::Enabled(enabled) => self.settings.enabled = enabled,
            Edit::OverwriteManual(enabled) => self.overwrite_manual = enabled,
            _ => {}
        }
    }

    fn rule_destination(&self, down: bool) -> Option<usize> {
        let index = self.selected_rule?;
        if down {
            (index + 1 < self.settings.rules.len()).then_some(index + 1)
        } else {
            index.checked_sub(1)
        }
    }

    fn rules_changed(&mut self) {
        self.rule_revision = self.rule_revision.wrapping_add(1);
    }

    fn change_rule(&mut self, index: usize, change: RuleChange) {
        let Some(rule) = self.settings.rules.get_mut(index) else {
            return;
        };
        match change {
            RuleChange::Enabled(enabled) => rule.enabled = enabled,
            RuleChange::Marker(id)
                if self
                    .settings
                    .definitions
                    .iter()
                    .any(|definition| definition.id == id) =>
            {
                rule.marker_id = id;
            }
            RuleChange::Pattern(pattern) if pattern.len() <= MAX_PATTERN_BYTES => {
                rule.pattern = pattern
            }
            RuleChange::Regex(enabled) => rule.regex = enabled,
            RuleChange::IgnoreCase(enabled) => rule.ignore_case = enabled,
            _ => return,
        }
        self.selected_rule = Some(index);
    }

    pub(super) fn view<'a>(
        &'a self,
        phase: Phase,
        blocker: Option<&'a str>,
        apply_blocker: Option<&'a str>,
    ) -> Element<'a, Event> {
        let busy = phase != Phase::Editing;
        let editable = !busy && blocker.is_none();
        let status = match phase {
            Phase::Preparing(Operation::Save) => Some("Saving markers…"),
            Phase::Preparing(Operation::Apply { .. }) | Phase::Applying => Some("Applying…"),
            Phase::Editing => blocker.or(self.status.as_deref()),
        };
        let grid = pane_grid(&self.panes, |_, section, _| {
            let pane = match section {
                Section::Definitions => self.definitions(editable),
                Section::Rules => self.rules(editable, apply_blocker),
            };
            pane_grid::Content::new(pane)
        })
        .spacing(6)
        .on_resize(8, |event| Event::Edit(Edit::Resize(event)));
        container(
            column![
                grid,
                rule::horizontal(1),
                row![
                    status.map(|status| text(status).size(13)),
                    space::horizontal(),
                    button("Cancel").on_press_maybe((!busy).then_some(Event::Cancel)),
                    button("OK").on_press_maybe(editable.then_some(Event::Save)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(8),
        )
        .padding(14)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }

    fn definitions(&self, editable: bool) -> Element<'_, Event> {
        let mut rows = column![].spacing(3);
        for definition in &self.settings.definitions {
            let selected = self.definition.as_deref() == Some(&definition.id);
            let id = definition.id.clone();
            let mut name = text_input("Name", &definition.name);
            if editable {
                name = name.on_input(move |value| Event::Edit(Edit::Name(id.clone(), value)));
            }
            rows = rows.push(
                row![
                    select_button(
                        selected,
                        editable,
                        Edit::SelectDefinition(definition.id.clone())
                    ),
                    self.color_cell(definition, editable),
                    name.width(Length::Fill),
                    text(severity(definition.legacy)).width(100),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        if self.settings.definitions.is_empty() {
            rows = rows.push(text("No custom markers.").size(13));
        }
        let selected = editable && self.definition.is_some();
        column![
            text("Markers").size(16),
            row![space().width(29), text("Colour").width(110), text("Name").width(Length::Fill), text("Counts as").width(100)]
                .spacing(6),
            scrollable(rows).height(Length::Fill),
            row![
                edit_button("Add", editable && self.settings.definitions.len() < MAX_DEFINITIONS, Edit::Add),
                edit_button("Duplicate", selected && self.settings.definitions.len() < MAX_DEFINITIONS, Edit::Duplicate),
                edit_button("Delete", selected, Edit::Delete),
            ].spacing(8),
            text("Journals keep a copy of the markers they use. Changes here affect future assignments.").size(12),
        ].spacing(5).height(Length::Fill).into()
    }

    fn color_cell(&self, definition: &Definition, editable: bool) -> Element<'_, Event> {
        let current = definition.color.map(Rgb::from_bytes);
        let label = current.map_or_else(|| "Default".into(), |color| color.to_string());
        let trigger = button(
            row![swatch(current), text(label).size(12)]
                .spacing(5)
                .align_y(Alignment::Center),
        )
        .width(110)
        .on_press_maybe(editable.then(|| Event::Edit(Edit::ShowColor(definition.id.clone()))))
        .into();
        let panel = self
            .color
            .as_ref()
            .filter(|color| editable && color.id == definition.id)
            .map(|color| {
                let id = definition.id.clone();
                let mut palette = row![].spacing(4);
                for color in PALETTE.map(Rgb::from_hex) {
                    palette = palette.push(
                        button(swatch(Some(color)))
                            .padding(4)
                            .on_press(Event::Edit(Edit::Color(id.clone(), color.to_string()))),
                    );
                }
                container(
                    column![
                        palette,
                        text_input("#RRGGBB", &color.text)
                            .on_input(move |value| Event::Edit(Edit::Color(id.clone(), value))),
                        (!color.text.is_empty() && Rgb::parse(&color.text).is_none())
                            .then(|| text("Enter a six-digit RGB colour.").size(12)),
                        edit_button("Done", true, Edit::CloseColor),
                    ]
                    .spacing(5),
                )
                .padding(8)
                .width(248)
                .style(container::bordered_box)
                .into()
            });
        popup::popover(trigger, panel, Event::Edit(Edit::CloseColor))
    }

    fn rules<'a>(&'a self, editable: bool, apply_blocker: Option<&'a str>) -> Element<'a, Event> {
        let mut rows = column![].spacing(3);
        for (index, rule) in self.settings.rules.iter().enumerate() {
            rows = rows.push(self.rule_row(index, rule, editable));
        }
        if self.settings.rules.is_empty() {
            rows = rows.push(text("No rules yet.").size(13));
        }
        let allowed = editable && apply_blocker.is_none();
        column![
            text("Automatic markers").size(16),
            toggle(
                self.settings.enabled,
                "Set markers automatically in new entries",
                editable,
                Edit::Enabled
            ),
            row![
                space().width(29),
                text("Active").size(12).width(54),
                text("Marker").size(12).width(142),
                text("Search term").size(12).width(Length::Fill),
                text("Regex").size(12).width(50),
                text("Ignore case").size(12).width(71),
            ]
            .spacing(5),
            scrollable(rows).height(Length::Fill),
            row![
                edit_button(
                    "Add rule",
                    editable
                        && !self.settings.definitions.is_empty()
                        && self.settings.rules.len() < MAX_RULES,
                    Edit::AddRule
                ),
                edit_button(
                    "Remove",
                    editable && self.selected_rule.is_some(),
                    Edit::RemoveRule
                ),
                edit_button(
                    "▲",
                    editable && self.rule_destination(false).is_some(),
                    Edit::MoveRule(false)
                ),
                edit_button(
                    "▼",
                    editable && self.rule_destination(true).is_some(),
                    Edit::MoveRule(true)
                ),
            ]
            .spacing(7),
            row![
                button("Apply now").on_press_maybe(allowed.then_some(Event::Apply)),
                toggle(
                    self.overwrite_manual,
                    "Also overwrite markers set by hand",
                    allowed,
                    Edit::OverwriteManual
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            apply_blocker.map(|message| text(message).size(12)),
            text("Rules are checked from top to bottom. The first matching rule applies.").size(12),
        ]
        .spacing(5)
        .height(Length::Fill)
        .into()
    }

    fn rule_row<'a>(&'a self, index: usize, rule: &'a Rule, editable: bool) -> Element<'a, Event> {
        let revision = self.rule_revision;
        let edit = move |change| Edit::ChangeRule(revision, index, change);
        let selected = self
            .settings
            .definitions
            .iter()
            .find(|definition| definition.id == rule.marker_id);
        let marker: Element<'_, Event> = if editable {
            pick_list(
                self.settings
                    .definitions
                    .iter()
                    .map(Choice)
                    .collect::<Vec<_>>(),
                selected.map(Choice),
                move |choice| Event::Edit(edit(RuleChange::Marker(choice.0.id.clone()))),
            )
            .width(124)
            .into()
        } else {
            text(selected.map_or("Missing marker", |definition| definition.name.as_str()))
                .width(124)
                .into()
        };
        let mut pattern = text_input("Search term", &rule.pattern);
        if editable {
            pattern = pattern.on_input(move |value| Event::Edit(edit(RuleChange::Pattern(value))));
        }
        row![
            select_button(
                self.selected_rule == Some(index),
                editable,
                Edit::SelectRule(revision, index)
            ),
            container(toggle(rule.enabled, "", editable, move |value| edit(
                RuleChange::Enabled(value)
            )))
            .width(54),
            row![
                swatch(selected.and_then(|definition| definition.color.map(Rgb::from_bytes))),
                marker
            ]
            .spacing(4)
            .width(142)
            .align_y(Alignment::Center),
            pattern.width(Length::Fill),
            container(toggle(rule.regex, "", editable, move |value| edit(
                RuleChange::Regex(value)
            )))
            .width(50),
            container(toggle(rule.ignore_case, "", editable, move |value| edit(
                RuleChange::IgnoreCase(value)
            )))
            .width(71),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .into()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Choice<'a>(&'a Definition);

impl fmt::Display for Choice<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.0.name)
    }
}

fn toggle(
    selected: bool,
    label: &'static str,
    editable: bool,
    on_toggle: impl Fn(bool) -> Edit + 'static,
) -> iced::widget::Checkbox<'static, Event> {
    let mut control = checkbox(selected).label(label);
    if editable {
        control = control.on_toggle(move |value| Event::Edit(on_toggle(value)));
    }
    control
}

fn severity(marker: LegacyMarker) -> &'static str {
    match marker {
        LegacyMarker::None => "None",
        LegacyMarker::Info => "Info",
        LegacyMarker::Important => "Important",
        LegacyMarker::Error => "Error",
    }
}

fn swatch(color: Option<Rgb>) -> Element<'static, Event> {
    container(space().width(13).height(13))
        .style(move |_| {
            color.map_or_else(container::Style::default, |color| {
                container::Style::default().background(color.color())
            })
        })
        .into()
}

fn edit_button(
    label: &'static str,
    enabled: bool,
    event: Edit,
) -> iced::widget::Button<'static, Event> {
    button(label).on_press_maybe(enabled.then_some(Event::Edit(event)))
}

fn select_button(
    selected: bool,
    enabled: bool,
    event: Edit,
) -> iced::widget::Button<'static, Event> {
    edit_button(if selected { "●" } else { "○" }, enabled, event)
        .width(29)
        .padding(3)
        .style(if selected {
            button::secondary
        } else {
            button::text
        })
}
