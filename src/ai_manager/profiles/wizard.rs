use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Length};

use super::{Editor, Field, Message, ModelMode, Profile, local_ai};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Open,
    Back,
    Next,
    Finish,
    Cancel,
}

#[derive(Clone, Copy)]
enum Step {
    Connection,
    Model,
    Name,
    Test,
}

impl Step {
    fn title(self) -> &'static str {
        match self {
            Self::Connection => "1. Local API connection",
            Self::Model => "2. Model",
            Self::Name => "3. Name",
            Self::Test => "4. Test and finish",
        }
    }
}

pub(super) struct Wizard {
    pub(super) profile: Profile,
    step: Step,
}

impl Editor {
    pub(super) fn update_wizard(&mut self, action: Action) {
        if matches!(action, Action::Open) {
            if let Some(profile) = self.new_profile() {
                self.clear_selection_feedback();
                self.wizard = Some(Wizard {
                    profile,
                    step: Step::Connection,
                });
            }
            return;
        }
        let Some(wizard) = &mut self.wizard else {
            return;
        };
        let outcome = match (action, wizard.step) {
            (Action::Back, Step::Model) => Ok(Step::Connection),
            (Action::Back, Step::Name) => Ok(Step::Model),
            (Action::Back, Step::Test) => Ok(Step::Name),
            (Action::Next, Step::Connection) => {
                local_ai::numeric_loopback_endpoint(&wizard.profile.endpoint)
                    .and_then(|_| wizard.profile.credential())
                    .map(|_| Step::Model)
            }
            (Action::Next, Step::Model)
                if wizard.profile.model_mode == ModelMode::Manual
                    && wizard.profile.model.trim().is_empty() =>
            {
                Err("Enter a model identifier or choose automatic model selection".into())
            }
            (Action::Next, Step::Model) => Ok(Step::Name),
            (Action::Next, Step::Name) => wizard.profile.validate().map(|()| Step::Test),
            (Action::Finish, Step::Test) => {
                if let Some(wizard) = self.wizard.take() {
                    self.insert_profile(wizard.profile);
                    self.set_status("Profile added to the draft. Choose Save to keep it.");
                }
                return;
            }
            (Action::Cancel, _) => {
                self.wizard = None;
                self.clear_selection_feedback();
                return;
            }
            _ => return,
        };
        match outcome {
            Ok(step) => {
                wizard.step = step;
                self.status = None;
            }
            Err(error) => self.set_status(error),
        }
    }

    pub(super) fn wizard_view<'a>(
        &'a self,
        wizard: &'a Wizard,
        editable: bool,
    ) -> Element<'a, Message> {
        let profile = &wizard.profile;
        let body = match wizard.step {
            Step::Connection => self.connection(profile, editable),
            Step::Model => self.model(profile, editable),
            Step::Name => text_input("Profile name", &profile.name)
                .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Name, value)))
                .into(),
            Step::Test => column![
                text(&profile.name),
                text(&profile.endpoint).size(13),
                text(if profile.model_mode == ModelMode::Auto {
                    "Automatic model selection"
                } else {
                    &profile.model
                })
                .size(13),
                text("Test the connection now or finish setup and test the profile later.")
                    .size(13),
                button("Test connection")
                    .style(button::secondary)
                    .on_press_maybe(editable.then_some(Message::Test)),
            ]
            .spacing(8)
            .into(),
        };
        let actions = row![
            button("Cancel")
                .style(button::secondary)
                .on_press_maybe(editable.then_some(Message::Wizard(Action::Cancel))),
            space::horizontal(),
            button("Back").style(button::secondary).on_press_maybe(
                (editable && !matches!(wizard.step, Step::Connection))
                    .then_some(Message::Wizard(Action::Back))
            ),
            if matches!(wizard.step, Step::Test) {
                button("Finish").on_press_maybe(editable.then_some(Message::Wizard(Action::Finish)))
            } else {
                button("Next").on_press_maybe(editable.then_some(Message::Wizard(Action::Next)))
            },
        ]
        .spacing(8);
        column![
            text("AI profile setup wizard").size(22),
            text(wizard.step.title()).size(16),
            self.status.as_deref().map(|status| text(status).size(13)),
            scrollable(container(body).padding(4)).height(Length::Fill),
            actions,
        ]
        .spacing(10)
        .into()
    }
}
