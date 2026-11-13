//! Edits the profile-local design catalog and previews an isolated sample run.

use iced::widget::{button, column, container, pick_list, row, text, text_input};
use iced::{Element, Length, Size, window};

use crate::terminal_agent::design::{
    Approval, Arrangement, Catalog, History, Layout, Preset, Status,
};
use crate::workspace::terminal_agent::design::{
    compose, history_slot, status_indicator, status_slot,
};

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
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Module {
    History(History),
    Status(Status),
    Approval(Approval),
    Arrangement(Arrangement),
}

#[derive(Default)]
pub(super) struct Editor {
    selected: Option<String>,
    name: Option<String>,
    removing: Option<String>,
    notice: &'static str,
    demo: preview::Demo,
}

impl Editor {
    fn selected<'a>(&'a self, catalog: &'a Catalog, preset: Preset) -> &'a str {
        self.selected
            .as_deref()
            .filter(|id| catalog.layout(id).is_some())
            .unwrap_or_else(|| catalog.default_id(preset))
    }

    pub(super) fn update(&mut self, catalog: &mut Catalog, preset: &mut Preset, edit: Edit) {
        if let Edit::Demo(action) = edit {
            self.demo.update(action);
            return;
        }
        let before = (
            self.selected(catalog, *preset).to_owned(),
            catalog.layout(self.selected(catalog, *preset)),
        );
        match edit {
            Edit::Select(id) if catalog.layout(&id).is_some() => {
                self.selected = Some(id);
                self.name = None;
                self.removing = None;
                self.notice = "";
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
                    self.notice = "Design removed. Connections that used it will use the default.";
                }
            }
            Edit::Module(id, change) if self.selected(catalog, *preset) == id => {
                if let Some(mut layout) = catalog.layout(&id) {
                    match change {
                        Module::History(value) => layout.history = value,
                        Module::Status(value) => layout.status = value,
                        Module::Approval(value) => layout.approval = value,
                        Module::Arrangement(value) => layout.arrangement = value,
                    }
                    catalog.set_layout(&id, layout);
                }
            }
            _ => {}
        }
        let after = (
            self.selected(catalog, *preset).to_owned(),
            catalog.layout(self.selected(catalog, *preset)),
        );
        if before != after {
            self.demo.replay();
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        catalog: &'a Catalog,
        preset: Preset,
        id: window::Id,
        target: Option<&iced::widget::Id>,
    ) -> Element<'a, Desktop> {
        let msg = move |edit| Desktop::Settings(Message::AgentDesignCatalog(id, edit));
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
        editor
            .push(text(self.notice).size(14))
            .push(preview::view(&self.demo, layout, id))
            .into()
    }
}

fn preview_static(
    layout: Layout,
    parts: Element<'static, Desktop>,
    terminal: Element<'static, Desktop>,
    phase: &'static str,
    stage: usize,
) -> Element<'static, Desktop> {
    let history_position = history_slot(layout.history);
    let (history, activity_window, floating) =
        if layout.arrangement == Arrangement::PictureInPicture {
            (None, None, Some(parts))
        } else if history_position.is_some() {
            (
                Some(
                    container(parts)
                        .padding(8)
                        .style(container::bordered_box)
                        .height(Length::Fill)
                        .width(Length::Fill)
                        .into(),
                ),
                None,
                None,
            )
        } else if layout.history.inline_style().is_some() {
            (None, None, None)
        } else {
            (None, Some(parts), None)
        };
    let status = if layout.arrangement == Arrangement::Adaptive {
        Status::Powerline
    } else {
        layout.status
    };
    let terminal = compose(
        terminal,
        history,
        status_indicator(status, phase, stage),
        history_position,
        status_slot(status),
        Size::new(650.0, 320.0),
        260.0,
    );
    let terminal = if let Some(floating) = floating {
        crate::workspace::terminal_agent::design::pip::preview(terminal, floating)
    } else {
        terminal
    };
    container(
        column![
            text("Design preview").size(17),
            text(layout.arrangement.to_string()).size(13),
            container(terminal).height(320).width(Length::Fill),
            activity_window,
        ]
        .spacing(7),
    )
    .style(container::bordered_box)
    .padding(10)
    .width(Length::Fill)
    .into()
}
