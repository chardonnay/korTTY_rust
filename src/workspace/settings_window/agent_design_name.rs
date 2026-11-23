//! The owned prompt for creating, duplicating and renaming design combinations.

use iced::widget::{Id, button, column, container, row, space, text, text_input};
use iced::{Alignment, Element, Length, Size, Task, Theme, window};

use crate::terminal_agent::design::{Catalog, MAX_NAME_BYTES, Preset};
use crate::workspace::dialog_design;

use super::{Desktop, DialogKind, Message, Section, Workspace, agent_design_settings::Edit};

pub(super) const TITLE: &str = "AI Agent Design";

#[derive(Clone, Debug)]
pub(crate) enum Operation {
    New,
    Duplicate(String),
    Rename(String),
}

impl Operation {
    pub(super) fn suggestion(&self, catalog: &Catalog) -> Option<String> {
        match self {
            Self::New => catalog.can_duplicate().then(|| "My combination".into()),
            Self::Duplicate(id) if catalog.can_duplicate() => catalog.copy_name(id),
            Self::Rename(id) if Preset::from_id(id).is_none() => catalog
                .choices()
                .into_iter()
                .find(|choice| choice.id == *id)
                .map(|choice| choice.name),
            _ => None,
        }
    }

    fn available(&self, catalog: &Catalog, name: &str) -> bool {
        let name = name.trim();
        match self {
            Self::New => catalog.can_duplicate() && catalog.available_name(name, None),
            Self::Duplicate(id) => {
                catalog.can_duplicate()
                    && catalog.layout(id).is_some()
                    && catalog.available_name(name, None)
            }
            Self::Rename(id) => {
                Preset::from_id(id).is_none()
                    && catalog.layout(id).is_some()
                    && catalog.available_name(name, Some(id))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Focus,
    Input(String),
    Submit,
}

pub(super) struct Prompt {
    pub(super) id: window::Id,
    pub(super) operation: Operation,
    pub(super) name: String,
    input: Id,
}

impl Prompt {
    pub(super) fn new(id: window::Id, operation: Operation, name: String) -> Self {
        Self {
            id,
            operation,
            name,
            input: Id::unique(),
        }
    }

    pub(super) fn focus(&self) -> Task<Desktop> {
        iced::widget::operation::focus(self.input.clone())
            .chain(iced::widget::operation::select_all(self.input.clone()))
    }

    pub(super) fn set_name(&mut self, mut name: String) {
        name.truncate(name.floor_char_boundary(MAX_NAME_BYTES));
        self.name = name;
    }

    pub(super) fn valid(&self, catalog: &Catalog) -> bool {
        self.operation.available(catalog, &self.name)
    }

    pub(super) fn view(&self, catalog: &Catalog) -> Element<'_, Desktop> {
        let id = self.id;
        let valid = self.valid(catalog);
        let submit = Desktop::Settings(Message::AgentDesignName(id, Action::Submit));
        let cancel = Desktop::Settings(Message::Cancel(id));
        let field = text_input("Combination name", &self.name)
            .id(self.input.clone())
            .size(13)
            .padding([5, 6])
            .style(dialog_design::form_input)
            .on_input(move |name| {
                Desktop::Settings(Message::AgentDesignName(id, Action::Input(name)))
            })
            .on_submit_maybe(valid.then(|| submit.clone()));
        let question = container(text("?").size(24).center())
            .center(40)
            .style(|theme: &Theme| container::Style {
                background: Some(theme.palette().primary.into()),
                text_color: Some(iced::Color::WHITE),
                border: iced::Border {
                    color: theme.palette().text,
                    width: 2.0,
                    radius: 20.0.into(),
                },
                ..container::Style::default()
            });
        let content = column![
            row![question, text("Name of the combination").size(13), field]
                .spacing(12)
                .align_y(Alignment::Center),
            row![
                space::horizontal(),
                button(text("OK").size(13).center())
                    .width(92)
                    .padding(6)
                    .on_press_maybe(valid.then_some(submit)),
                button(text("Cancel").size(13).center())
                    .width(92)
                    .padding(6)
                    .style(dialog_design::secondary_button)
                    .on_press(cancel.clone()),
            ]
            .spacing(10),
        ]
        .spacing(12)
        .padding(12)
        .width(Length::Fill);
        super::on_escape(content.into(), cancel)
    }
}

impl Workspace {
    pub(super) fn agent_design_name_for(&self, owner: window::Id) -> Option<window::Id> {
        self.settings
            .0
            .as_ref()
            .filter(|dialog| dialog.id == owner)?
            .agent_design_name
            .as_ref()
            .map(|prompt| prompt.id)
    }

    pub(super) fn open_agent_design_name(
        &mut self,
        owner: window::Id,
        operation: Operation,
    ) -> Task<Desktop> {
        if let Some(id) = self.agent_design_name_for(owner) {
            return window::gain_focus(id);
        }
        let Some(dialog) = self
            .settings
            .dialog_mut(owner)
            .filter(|dialog| dialog.section == Section::AgentDesign)
        else {
            return Task::none();
        };
        let Some(name) = operation.suggestion(&dialog.draft.preferences.ai_agent_designs) else {
            return Task::none();
        };
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            DialogKind::AgentDesignName,
            window::Settings {
                size: Size::new(400.0, 112.0),
                min_size: Some(Size::new(380.0, 112.0)),
                resizable: false,
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(|id| Desktop::Settings(Message::AgentDesignName(id, Action::Focus))),
        );
        if let Some(dialog) = self.settings.dialog_mut(owner) {
            dialog.agent_design_name = Some(Prompt::new(id, operation, name));
        }
        opened
    }

    pub(super) fn update_agent_design_name(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Desktop> {
        let Some(dialog) = self.settings.0.as_mut() else {
            return Task::none();
        };
        let Some(prompt) = dialog
            .agent_design_name
            .as_mut()
            .filter(|prompt| prompt.id == id)
        else {
            return Task::none();
        };
        match action {
            Action::Focus => prompt.focus(),
            Action::Input(name) => {
                prompt.set_name(name);
                Task::none()
            }
            Action::Submit if prompt.valid(&dialog.draft.preferences.ai_agent_designs) => {
                let owner = dialog.id;
                let prompt = dialog
                    .agent_design_name
                    .take()
                    .expect("matched name dialog");
                let apply = self.update_settings(Message::AgentDesignCatalog(
                    owner,
                    Edit::ApplyName(prompt.operation, prompt.name),
                ));
                self.close_placed_dialog_and_focus(id, owner).chain(apply)
            }
            Action::Submit => Task::none(),
        }
    }

    pub(super) fn close_agent_design_name(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.settings.0.as_mut()?;
        if dialog.agent_design_name.as_ref()?.id != id {
            return None;
        }
        dialog.agent_design_name = None;
        let owner = dialog.id;
        Some(self.close_placed_dialog_and_focus(id, owner))
    }
}
