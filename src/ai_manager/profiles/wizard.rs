use std::net::SocketAddr;
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Length, Task};

use crate::ai_manager::search::Reveal;
use crate::local_ai::{self, LocalModelServer};

use super::{ConnectionMode, Editor, Field, Message, ModelMode, Profile};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Open,
    Back,
    Next,
    Finish,
    Cancel,
    Scan,
    CancelScan,
    UseServer(SocketAddr),
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
            Self::Connection => "1. Connection",
            Self::Model => "2. Model",
            Self::Name => "3. Name",
            Self::Test => "4. Test and finish",
        }
    }
}

pub(super) struct Wizard {
    pub(super) profile: Profile,
    step: Step,
    scan: Option<Scan>,
}

enum Scan {
    Pending {
        ticket: Arc<()>,
        _cancel: local_ai::Cancel,
    },
    Complete(Arc<Vec<LocalModelServer>>),
}

#[derive(Clone)]
pub(crate) struct ScanResult {
    ticket: Arc<()>,
    result: Result<Arc<Vec<LocalModelServer>>, String>,
}

impl Editor {
    pub(super) fn update_wizard(&mut self, action: Action) {
        if matches!(action, Action::Open) {
            if let Some(profile) = self.new_profile() {
                self.clear_selection_feedback();
                self.wizard = Some(Wizard {
                    profile,
                    step: Step::Connection,
                    scan: None,
                });
            }
            return;
        }
        let Some(wizard) = &mut self.wizard else {
            return;
        };
        let outcome = match (action, wizard.step) {
            (Action::CancelScan, Step::Connection) => {
                wizard.scan = None;
                return;
            }
            (Action::UseServer(endpoint), Step::Connection) => {
                self.use_scanned_server(endpoint);
                return;
            }
            (Action::Back, Step::Model) => Ok(Step::Connection),
            (Action::Back, Step::Name) => Ok(Step::Model),
            (Action::Back, Step::Test) => Ok(Step::Name),
            (Action::Next, Step::Connection) => {
                wizard.profile.validate_connection().map(|()| Step::Model)
            }
            (Action::Next, Step::Model)
                if wizard.profile.is_managed() || wizard.profile.is_cli() =>
            {
                wizard.profile.validate_model().map(|()| Step::Name)
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
                if matches!(wizard.scan, Some(Scan::Pending { .. })) {
                    wizard.scan = None;
                }
                wizard.step = step;
                self.cancel_cli_catalog();
                self.cli_discovery = None;
                self.status = None;
            }
            Err(error) => self.set_status(error),
        }
    }

    pub(in crate::ai_manager) fn scan_wizard_servers(&mut self) -> Task<Message> {
        let Some(wizard) = self.wizard.as_mut().filter(|wizard| {
            matches!(wizard.step, Step::Connection)
                && !matches!(wizard.scan, Some(Scan::Pending { .. }))
        }) else {
            return Task::none();
        };
        let mut addresses = ["127.0.0.1:1234", "127.0.0.1:11434"]
            .map(|endpoint| endpoint.parse().expect("fixed loopback address"))
            .to_vec();
        for profile in std::iter::once(&wizard.profile).chain(self.catalog.entries()) {
            if addresses.len() >= local_ai::MAX_LOCAL_SCAN_ENDPOINTS {
                break;
            }
            if let Ok(address) = match profile.connection_mode {
                ConnectionMode::External => local_ai::numeric_loopback_endpoint(&profile.endpoint),
                ConnectionMode::LmStudio => local_ai::lm_studio_endpoint(&profile.endpoint),
                _ => continue,
            } && !addresses.contains(&address)
            {
                addresses.push(address);
            }
        }
        let pending = match local_ai::scan_local_models(addresses) {
            Ok(pending) => pending,
            Err(error) => {
                self.set_status(error);
                return Task::none();
            }
        };
        let ticket = Arc::new(());
        wizard.scan = Some(Scan::Pending {
            ticket: Arc::clone(&ticket),
            _cancel: pending.cancel,
        });
        self.status = None;
        Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The local model discovery worker stopped.".into()))
                    .map(Arc::new)
            },
            move |result| Message::WizardScanFinished(ScanResult { ticket, result }),
        )
    }

    pub(in crate::ai_manager) fn finish_wizard_scan(&mut self, result: ScanResult, editable: bool) {
        let Some(wizard) = self.wizard.as_mut().filter(|wizard| {
            matches!(&wizard.scan, Some(Scan::Pending { ticket, .. }) if Arc::ptr_eq(ticket, &result.ticket))
        }) else { return; };
        wizard.scan = None;
        if !editable {
            return;
        }
        match result.result {
            Ok(servers) => wizard.scan = Some(Scan::Complete(servers)),
            Err(error) => self.set_status(error),
        }
    }

    fn use_scanned_server(&mut self, endpoint: SocketAddr) {
        let Some(server) = self.wizard.as_ref().and_then(|wizard| match &wizard.scan {
            Some(Scan::Complete(servers)) => servers
                .iter()
                .find(|server| server.endpoint == endpoint)
                .cloned(),
            _ => None,
        }) else {
            return;
        };
        let native_lm_studio = self
            .selected()
            .into_iter()
            .chain(self.catalog.entries())
            .any(|profile| {
                profile.connection_mode == ConnectionMode::LmStudio
                    && local_ai::lm_studio_endpoint(&profile.endpoint).ok() == Some(endpoint)
            });
        let mode = if native_lm_studio || endpoint.port() == 1234 {
            ConnectionMode::LmStudio
        } else {
            ConnectionMode::External
        };
        self.select_connection_mode(mode);
        let endpoint = endpoint.to_string();
        self.edit(Field::Endpoint, endpoint.clone());
        if let Some(id) = self.selected().map(|profile| profile.id)
            && let Ok(models) = server.models
        {
            self.set_models(id, &endpoint, models);
        }
    }

    pub(super) fn wizard_view<'a>(
        &'a self,
        wizard: &'a Wizard,
        editable: bool,
        form_lock: Option<&'static str>,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let profile = &wizard.profile;
        let body = match wizard.step {
            Step::Connection => column![
                (editable && profile.remote_protocol().is_none()).then(|| super::view::cloud_provider_picker(profile)),
                self.connection(profile, editable, reveal),
                self.local_scan_view(wizard, editable),
            ].spacing(10).into(),
            Step::Model => self.model(profile, editable, reveal),
            Step::Name => text_input("Profile name", &profile.name)
                .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Name, value)))
                .into(),
            Step::Test => column![
                text(&profile.name),
                text(
                    profile
                        .destination()
                        .unwrap_or_else(|_| profile.endpoint.clone())
                )
                .size(13),
                text(if profile.model_mode == ModelMode::Auto {
                    "Automatic model selection"
                } else {
                    &profile.model
                })
                .size(13),
                text(if profile.is_cli() {
                    "The fixed test runs the approved program and provides only the test message through the configured CLI input."
                } else {
                    "Test the connection now or finish setup and test the profile later."
                })
                    .size(13),
                button(if profile.is_cli() { "Run fixed CLI test" } else { "Test connection" })
                    .style(button::secondary)
                    .on_press_maybe(editable.then_some(Message::Test)),
            ]
            .spacing(8)
            .into(),
        };
        let actions = row![
            button("Cancel")
                .style(button::secondary)
                .on_press(Message::Wizard(Action::Cancel)),
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
            form_lock.map(|notice| text(notice).size(13)),
            self.status.as_deref().map(|status| text(status).size(13)),
            scrollable(container(body).padding(4)).height(Length::Fill),
            actions,
        ]
        .spacing(10)
        .into()
    }

    fn local_scan_view<'a>(&self, wizard: &'a Wizard, editable: bool) -> Element<'a, Message> {
        if matches!(&wizard.scan, Some(Scan::Pending { .. })) {
            return row![
                text("Checking local model servers…"),
                button("Cancel scan").on_press(Message::Wizard(Action::CancelScan))
            ]
            .spacing(8)
            .into();
        }
        let mut details = column![
            button("Find local model servers")
                .on_press_maybe(editable.then_some(Message::Wizard(Action::Scan)))
        ]
        .spacing(8);
        if let Some(Scan::Complete(servers)) = &wizard.scan {
            for server in servers.iter() {
                let endpoint = server.endpoint;
                let status = match &server.models {
                    Ok(models) if models.is_empty() => "No models listed".into(),
                    Ok(models) => format!("{} models", models.len()),
                    Err(error) => error.clone(),
                };
                details = details.push(
                    row![
                        text(format!("{endpoint} · {status}")).size(13),
                        button("Use endpoint").on_press_maybe(
                            editable.then_some(Message::Wizard(Action::UseServer(endpoint)))
                        )
                    ]
                    .spacing(8)
                    .wrap(),
                );
            }
        }
        details.into()
    }
}
