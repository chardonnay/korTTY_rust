use std::net::SocketAddr;
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Length, Task};

use crate::ai_manager::search::{ProfileSetting, Reveal, Setting};
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

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
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

impl Wizard {
    pub(super) fn model_step_open(&self) -> bool {
        matches!(self.step, Step::Model)
    }

    fn next_step(&self, step: Step) -> Result<Step, String> {
        match step {
            Step::Connection => self.profile.validate_connection().map(|()| Step::Model),
            Step::Model if self.profile.is_managed() || self.profile.is_cli() => {
                self.profile.validate_model().map(|()| Step::Name)
            }
            Step::Model
                if self.profile.model_mode == ModelMode::Manual
                    && self.profile.model.trim().is_empty() =>
            {
                Err("Enter a model identifier or choose automatic model selection".into())
            }
            Step::Model => Ok(Step::Name),
            Step::Name => self.profile.validate().map(|()| Step::Test),
            Step::Test => Ok(Step::Test),
        }
    }

    fn reachable_step(&self, editable: bool) -> Step {
        let mut step = self.step;
        while editable && step != Step::Test {
            let Ok(next) = self.next_step(step) else {
                break;
            };
            step = next;
        }
        step
    }

    fn setting_step(&self, setting: Setting) -> Option<Step> {
        match setting {
            Setting::WizardScan => Some(Step::Connection),
            Setting::WizardTest => Some(Step::Test),
            Setting::Profile(ProfileSetting::Web(_)) => None,
            Setting::Profile(ProfileSetting::Name) => Some(Step::Name),
            Setting::Profile(ProfileSetting::CloudProvider)
                if self.profile.remote_protocol().is_none() =>
            {
                Some(Step::Connection)
            }
            Setting::Profile(setting)
                if setting != ProfileSetting::Default && setting.visible(&self.profile) =>
            {
                match setting.section() {
                    super::Section::Connection => Some(Step::Connection),
                    super::Section::Model => Some(Step::Model),
                    _ => None,
                }
            }
            _ => None,
        }
    }
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
    pub(in crate::ai_manager) fn wizard_search_settings(
        &self,
        editable: bool,
    ) -> impl Iterator<Item = Setting> + '_ {
        self.wizard.iter().flat_map(move |wizard| {
            let reachable = wizard.reachable_step(editable);
            ProfileSetting::ALL
                .iter()
                .copied()
                .map(Setting::Profile)
                .chain([Setting::WizardScan, Setting::WizardTest])
                .filter(move |&setting| {
                    (editable || setting != Setting::Profile(ProfileSetting::CloudProvider))
                        && wizard.setting_step(setting).is_some_and(|step| {
                            step == wizard.step || editable && step <= reachable
                        })
                })
        })
    }

    pub(in crate::ai_manager) fn reveal_wizard_setting(
        &mut self,
        setting: Setting,
        editable: bool,
    ) -> bool {
        let Some(wizard) = &self.wizard else {
            return false;
        };
        let Some(step) = wizard.setting_step(setting) else {
            return false;
        };
        if !editable
            && (step != wizard.step || setting == Setting::Profile(ProfileSetting::CloudProvider))
            || step > wizard.reachable_step(editable)
        {
            return false;
        }
        if step != wizard.step {
            self.set_wizard_step(step);
        }
        true
    }

    fn set_wizard_step(&mut self, step: Step) {
        let Some(wizard) = &mut self.wizard else {
            return;
        };
        if matches!(wizard.scan, Some(Scan::Pending { .. })) {
            wizard.scan = None;
        }
        wizard.step = step;
        self.cancel_cli_catalog();
        self.cli_discovery = None;
        self.status = None;
    }

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
            (Action::Next, step) if step != Step::Test => wizard.next_step(step),
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
            Ok(step) => self.set_wizard_step(step),
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
            if (profile.connection_mode == ConnectionMode::External
                || profile.connection_mode.uses_lm_studio_inventory())
                && let Ok(address) = profile.local_address()
                && !addresses.contains(&address)
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
        let known_lm_studio = self
            .selected()
            .into_iter()
            .chain(self.catalog.entries())
            .find(|profile| {
                profile.connection_mode.uses_lm_studio_inventory()
                    && profile.local_address().ok() == Some(endpoint)
            })
            .map(|profile| profile.connection_mode);
        let mode = known_lm_studio.unwrap_or(if endpoint.port() == 1234 {
            ConnectionMode::LmStudio
        } else {
            ConnectionMode::External
        });
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
                (editable && profile.remote_protocol().is_none()).then(|| reveal.wrap(
                    Setting::Profile(ProfileSetting::CloudProvider),
                    super::view::cloud_provider_picker(profile),
                )),
                self.connection(profile, editable, reveal),
                reveal.wrap(Setting::WizardScan, self.local_scan_view(wizard, editable)),
            ].spacing(10).into(),
            Step::Model => self.model(profile, editable, reveal),
            Step::Name => {
                let setting = Setting::Profile(ProfileSetting::Name);
                let name = text_input("Profile name", &profile.name)
                    .on_input_maybe(editable.then_some(|value| Message::Edit(Field::Name, value)));
                reveal.wrap(setting, reveal.input(setting, editable, name))
            }
            Step::Test => column![
                text(&profile.name),
                text(
                    profile
                        .destination()
                        .unwrap_or_else(|_| profile.endpoint.clone())
                )
                .size(13),
                text(if profile.ordinary_lm_studio_auto() && !profile.effective_model().is_empty() {
                    profile.effective_model()
                } else if profile.model_mode == ModelMode::Auto {
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
                reveal.action(Setting::WizardTest, button(if profile.is_cli() { "Run fixed CLI test" } else { "Test connection" })
                    .style(button::secondary)
                    .on_press_maybe(editable.then_some(Message::Test))),
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
            scrollable(container(body).padding(4))
                .id(reveal.viewport.clone())
                .height(Length::Fill),
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
