//! Edits the profile-local design catalog and previews an isolated sample run.

use iced::widget::{button, checkbox, column, pick_list, row, slider, text, text_input};
use iced::{Element, window};

use crate::terminal_agent::design::{
    Approval, Arrangement, Catalog, History, Layout, Preset, Status,
};
use crate::workspace::tab_titles::WindowSettings;
use crate::workspace::terminal_agent::design::{AUTO_HIDE_SECONDS, DockPlacement, DockPreferences};

use super::{Desktop, Message};

mod preview;

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Demo(preview::Action),
    Select(String),
    Default(String),
    Duplicate(String),
    Name(String, String),
    Rename(String),
    Remove(String),
    ConfirmRemove(String, bool),
    Module(String, Module),
    Preview(Option<Module>),
    DockPlacement(DockPlacement),
    DockWidth(u16),
    AutoHide(u8),
    ReducedAnimations(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Module {
    History(History),
    Status(Status),
    Approval(Approval),
    Arrangement(Arrangement),
}

impl Module {
    fn apply(self, layout: &mut Layout) {
        match self {
            Self::History(value) => layout.history = value,
            Self::Status(value) => layout.status = value,
            Self::Approval(value) => layout.approval = value,
            Self::Arrangement(value) => layout.arrangement = value,
        }
    }

    fn choices() -> Vec<Self> {
        History::ALL
            .into_iter()
            .map(Self::History)
            .chain(Status::ALL.into_iter().map(Self::Status))
            .chain(Approval::ALL.into_iter().map(Self::Approval))
            .chain(Arrangement::ALL.into_iter().map(Self::Arrangement))
            .collect()
    }
}

impl std::fmt::Display for Module {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::History(value) => write!(formatter, "History · {value}"),
            Self::Status(value) => write!(formatter, "Status · {value}"),
            Self::Approval(value) => write!(formatter, "Approval · {value}"),
            Self::Arrangement(value) => write!(formatter, "Arrangement · {value}"),
        }
    }
}

#[derive(Default)]
pub(super) struct Editor {
    selected: Option<String>,
    name: Option<String>,
    removing: Option<String>,
    notice: &'static str,
    demo: preview::Demo,
    preview_module: Option<Module>,
}

impl Editor {
    fn selected<'a>(&'a self, catalog: &'a Catalog, preset: Preset) -> &'a str {
        self.selected
            .as_deref()
            .filter(|id| catalog.layout(id).is_some())
            .unwrap_or_else(|| catalog.default_id(preset))
    }

    fn preview_layout(&self, catalog: &Catalog, preset: Preset) -> Layout {
        let mut layout = catalog
            .layout(self.selected(catalog, preset))
            .unwrap_or_default();
        if let Some(module) = self.preview_module {
            module.apply(&mut layout);
        }
        layout
    }

    pub(super) fn update(&mut self, catalog: &mut Catalog, preset: &mut Preset, edit: Edit) {
        if let Edit::Demo(action) = edit {
            self.demo.update(action);
            return;
        }
        let before = (
            self.selected(catalog, *preset).to_owned(),
            self.preview_layout(catalog, *preset),
        );
        match edit {
            Edit::Select(id) if catalog.layout(&id).is_some() => {
                self.selected = Some(id);
                self.name = None;
                self.removing = None;
                self.notice = "";
                self.preview_module = None;
            }
            Edit::Default(id) => {
                if let Some(selected) = Preset::from_id(&id) {
                    *preset = selected;
                    catalog.clear_custom_default();
                } else if catalog.set_custom_default(&id) {
                    *preset = Preset::Standard;
                }
            }
            Edit::Duplicate(id) => {
                if let Some(choice) = catalog.duplicate(&id) {
                    self.selected = Some(choice.id);
                    self.name = None;
                    self.removing = None;
                    self.notice = "";
                    self.preview_module = None;
                }
            }
            Edit::Name(id, name) if self.selected(catalog, *preset) == id => {
                self.selected = Some(id);
                self.name = Some(
                    name[..name.floor_char_boundary(crate::terminal_agent::design::MAX_NAME_BYTES)]
                        .to_owned(),
                );
                self.notice = "";
            }
            Edit::Rename(id) if self.selected(catalog, *preset) == id => {
                if let Some(name) = &self.name {
                    if catalog.rename(&id, name) {
                        self.name = None;
                        self.notice = "Design renamed.";
                    } else {
                        self.notice = "Enter a unique design name without control characters.";
                    }
                }
            }
            Edit::Remove(id)
                if self.selected(catalog, *preset) == id && Preset::from_id(&id).is_none() =>
            {
                self.selected = Some(id.clone());
                self.removing = Some(id);
            }
            Edit::ConfirmRemove(id, confirmed) if self.removing.as_deref() == Some(id.as_str()) => {
                self.removing = None;
                if confirmed {
                    catalog.remove(&id);
                    self.selected = None;
                    self.name = None;
                    self.preview_module = None;
                    self.notice = "Design removed. Connections that used it will use the default.";
                }
            }
            Edit::Module(id, change) if self.selected(catalog, *preset) == id => {
                if let Some(mut layout) = catalog.layout(&id) {
                    change.apply(&mut layout);
                    catalog.set_layout(&id, layout);
                    self.preview_module = None;
                }
            }
            Edit::Preview(module) => self.preview_module = module,
            _ => {}
        }
        let after = (
            self.selected(catalog, *preset).to_owned(),
            self.preview_layout(catalog, *preset),
        );
        if before != after {
            self.demo.replay();
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        preferences: &'a WindowSettings,
        id: window::Id,
        target: Option<&iced::widget::Id>,
    ) -> Element<'a, Desktop> {
        let msg = move |edit| Desktop::Settings(Message::AgentDesignCatalog(id, edit));
        let catalog = &preferences.ai_agent_designs;
        let preset = preferences.ai_agent_design;
        let dock = preferences.ai_agent_panel;
        let choices = catalog.choices();
        let default = choices
            .iter()
            .find(|choice| choice.id == catalog.default_id(preset))
            .cloned();
        let selected = self.selected(catalog, preset);
        let choice = choices.iter().find(|choice| choice.id == selected).cloned();
        let layout = catalog.layout(selected).unwrap_or_default();
        let mut editor = column![
            text("Default AI agent design"),
            pick_list(choices.clone(), default, move |choice| msg(Edit::Default(choice.id))),
            text("Applies to newly opened agent activity. Saved connections can choose their own design.").size(14),
            text("Agent panel placement"),
            pick_list(DockPlacement::ALL, Some(dock.placement.unwrap_or_default()), move |placement| {
                msg(Edit::DockPlacement(placement))
            }),
            text(format!("Agent panel side width ({} px)", dock.width())),
            slider(DockPreferences::WIDTH_RANGE, dock.width(), move |width| msg(Edit::DockWidth(width))),
            text("Design to edit"),
            pick_list(choices, choice.clone(), move |choice| msg(Edit::Select(choice.id))),
            row![
                button("New design").on_press_maybe(catalog.can_duplicate().then(|| msg(Edit::Duplicate(Preset::Standard.id().into())))),
                button("Duplicate selected").on_press_maybe(catalog.can_duplicate().then(|| msg(Edit::Duplicate(selected.into())))),
            ].spacing(8).wrap(),
        ].spacing(8);
        if let Some(preset) = Preset::from_id(selected) {
            editor = editor.push(text(preset.description()).size(14));
        } else if let Some(choice) = &choice {
            let name = self.name.as_deref().unwrap_or(&choice.name);
            editor = editor.push(
                row![
                    crate::settings_search::mark_input(
                        text_input("Design name", name)
                            .on_input(move |name| msg(Edit::Name(selected.into(), name))),
                        target,
                    ),
                    button("Rename").on_press_maybe(
                        self.name
                            .is_some()
                            .then(|| msg(Edit::Rename(selected.into())))
                    ),
                    button("Remove…").on_press(msg(Edit::Remove(selected.into()))),
                ]
                .spacing(8)
                .wrap(),
            );
            if self.removing.as_deref() == Some(selected) {
                editor = editor.push(
                    column![
                        text(format!(
                            "Remove {}? Connections that use it will use the default.",
                            choice.name
                        )),
                        row![
                            button("Remove design")
                                .on_press(msg(Edit::ConfirmRemove(selected.into(), true))),
                            button("Keep design")
                                .on_press(msg(Edit::ConfirmRemove(selected.into(), false))),
                        ]
                        .spacing(8),
                    ]
                    .spacing(5),
                );
            }
            editor = editor.push(
                column![
                    text("Activity history").size(14),
                    pick_list(History::ALL, Some(layout.history), move |value| msg(
                        Edit::Module(selected.into(), Module::History(value))
                    )),
                    text("Terminal status").size(14),
                    pick_list(Status::ALL, Some(layout.status), move |value| msg(
                        Edit::Module(selected.into(), Module::Status(value))
                    )),
                    text("Approval controls").size(14),
                    pick_list(Approval::ALL, Some(layout.approval), move |value| msg(
                        Edit::Module(selected.into(), Module::Approval(value))
                    )),
                    text("Arrangement").size(14),
                    pick_list(
                        Arrangement::ALL,
                        Some(layout.arrangement),
                        move |value| msg(Edit::Module(selected.into(), Module::Arrangement(value)))
                    ),
                ]
                .spacing(5),
            );
        }
        let delay = preferences.agent_status_auto_hide_seconds();
        if !self.notice.is_empty() {
            editor = editor.push(text(self.notice).size(14));
        }
        editor
            .push(text(if delay == 0 {
                "Keep status visible after a run".to_owned()
            } else {
                format!("Hide status {delay} s after a run")
            }))
            .push(slider(AUTO_HIDE_SECONDS, delay, move |seconds| {
                msg(Edit::AutoHide(seconds))
            }))
            .push(
                checkbox(
                    preferences
                        .ai_agent_design_reduced_animations
                        .unwrap_or_default(),
                )
                .label("Reduce Agent design animations")
                .on_toggle(move |reduced| msg(Edit::ReducedAnimations(reduced))),
            )
            .push(
                row![
                    pick_list(Module::choices(), self.preview_module, move |module| {
                        msg(Edit::Preview(Some(module)))
                    })
                    .placeholder("Preview a module"),
                    button("Reset preview")
                        .on_press_maybe(self.preview_module.map(|_| msg(Edit::Preview(None)))),
                ]
                .spacing(6)
                .wrap(),
            )
            .push(preview::view(
                &self.demo,
                self.preview_layout(catalog, preset),
                dock,
                delay,
                !preferences.agent_animations(),
                id,
            ))
            .into()
    }
}
