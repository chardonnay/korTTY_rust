//! A separate generation review. Generated code is returned as an unsaved snippet.

use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length, Task, window};

use crate::ai_manager::Configuration;
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai;
use crate::snippet_analysis::model::hardening::{MAX_FILE_SIZE_MIB, Preferences};
use crate::ssh_dialog::LoginTarget;
use crate::terminal_agent::swarm::workflow::{
    self as engine, Draft, Header, HeaderFacts, Hosts, Language, Options, Request,
};
use crate::terminal_logging::{Policy, Redactor, Secrets};

use super::super::request_review::{CliPresentation, OutboundAuthority};
use super::{
    Desktop, Group, Message as GroupMessage, ProfileChoice, RunMessage, SwarmMode, Workspace, mask,
    require_swarm_policy, swarm_profile_entries,
};

#[derive(Clone)]
pub enum Message {
    Language(Language),
    HostSource(HostSource),
    Hosts(text_editor::Action),
    Inventory(String),
    Instructions(String),
    Parallelism(String),
    Timeout(String),
    Retries(String),
    Jump(String),
    FileSize(String),
    Toggle(Flag, bool),
    Review,
    Generate(u64),
    Reply(u64, Arc<Result<Draft, String>>),
    Cancel,
    OpenSnippet(u64),
    Back,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum HostSource {
    #[default]
    Selected,
    Manual,
    Inventory,
}

impl std::fmt::Display for HostSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Selected => "Selected SSH panes",
            Self::Manual => "Manually entered SSH destinations",
            Self::Inventory => "Inventory path",
        })
    }
}

#[derive(Clone, Copy)]
pub enum Flag {
    ContinueOnError,
    CollectResults,
    Aggregate,
    Strict,
    DryRun,
    Sudo,
    InputHardening,
}

pub(super) struct Form {
    language: Language,
    host_source: HostSource,
    hosts: text_editor::Content,
    inventory: String,
    instructions: String,
    parallelism: String,
    timeout: String,
    retries: String,
    jump: String,
    options: Options,
    hardening: Preferences,
    file_size: String,
    stage: Stage,
    epoch: u64,
    generation: u64,
}

enum Stage {
    Setup,
    Review {
        profile_display: String,
        authority: Authority,
        system: String,
        input: String,
        cli: Option<Box<CliPresentation>>,
        header: Header,
    },
    Waiting {
        _cancel: local_ai::Cancel,
        authority: Authority,
    },
    Generated(Draft),
}

#[derive(Clone)]
pub(super) struct Authority {
    configuration: Arc<Configuration>,
    profile: ProfileChoice,
    transport: local_ai::Profile,
    limit: usize,
    redactions: Policy,
    outbound: Option<OutboundAuthority>,
}

impl Authority {
    fn require_current(&self, live: &Configuration, workspace: &Workspace) -> Result<(), String> {
        if self.configuration.saved_root() != live.saved_root()
            || !self
                .redactions
                .same_redactions(&workspace.clipboard.policy.terminal_logging)
            || !self.matches_profile(live)
            || self
                .outbound
                .as_ref()
                .is_some_and(|outbound| !outbound.matches_vault(workspace.vault.as_ref()))
        {
            return Err(
                "The workflow profile or redaction rules changed. Review the request again.".into(),
            );
        }
        if let Some(outbound) = &self.outbound {
            outbound.check_policy(&workspace.clipboard.policy)?;
        }
        self.require_dispatch()
    }

    fn matches_profile(&self, config: &Configuration) -> bool {
        config.enabled()
            && self
                .profile
                .transport(config)
                .is_ok_and(|(transport, limit)| transport == self.transport && limit == self.limit)
    }

    fn require_dispatch(&self) -> Result<(), String> {
        self.transport.require_current_policy_authority()?;
        if let Some(outbound) = &self.outbound {
            outbound.check(&self.transport)?;
        }
        let policy = ClipboardPolicy::load();
        require_swarm_policy(&policy, SwarmMode::Advisory)?;
        if let Some(outbound) = &self.outbound {
            outbound.check_policy(&policy)?;
        }
        if !self
            .redactions
            .equivalent_redactions(&policy.terminal_logging)
            || !self
                .configuration
                .reload_saved()
                .is_ok_and(|saved| self.matches_profile(&saved))
        {
            return Err(
                "The saved workflow profile or redaction rules changed. Review the request again."
                    .into(),
            );
        }
        Ok(())
    }

    fn mask_text(
        &self,
        redactors: &[Redactor],
        value: &str,
        workspace: &Workspace,
    ) -> Result<String, String> {
        let value = mask_text(redactors, value)?;
        if let Some(outbound) = &self.outbound {
            outbound.mask_text(&value, workspace.vault.as_ref())
        } else {
            Ok(value)
        }
    }
}

impl Default for Form {
    fn default() -> Self {
        let options = Options::default();
        let hardening = Preferences::default();
        Self {
            language: Language::default(),
            host_source: HostSource::default(),
            hosts: text_editor::Content::new(),
            inventory: String::new(),
            instructions: String::new(),
            parallelism: String::new(),
            timeout: options.timeout_seconds.to_string(),
            retries: options.retries.to_string(),
            jump: String::new(),
            options,
            file_size: hardening.max_file_size_mib.to_string(),
            hardening,
            stage: Stage::Setup,
            epoch: 0,
            generation: 0,
        }
    }
}

impl Form {
    pub(super) fn new(generation: u64) -> Self {
        Self {
            generation,
            ..Self::default()
        }
    }

    pub(super) fn running(&self) -> bool {
        matches!(self.stage, Stage::Waiting { .. })
    }

    pub(super) fn draft(&self, epoch: u64) -> Option<&Draft> {
        match &self.stage {
            Stage::Generated(draft) if epoch == self.epoch => Some(draft),
            _ => None,
        }
    }

    pub(super) fn request(
        &self,
        goal: String,
        selected: Vec<LoginTarget>,
        mask: impl Fn(&str) -> Result<String, String>,
    ) -> Result<String, String> {
        let hosts = match self.host_source {
            HostSource::Selected => Hosts::Selected(selected),
            HostSource::Manual => Hosts::Manual(
                self.hosts
                    .text()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(parse_login)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            HostSource::Inventory => Hosts::Inventory(PathBuf::from(self.inventory.trim())),
        };
        let mut options = self.options.clone();
        let mut hardening = self.hardening.clone();
        if hardening.input_enabled && self.language != Language::Ansible {
            hardening.max_file_size_mib = self
                .file_size
                .parse::<u16>()
                .ok()
                .filter(|value| *value <= MAX_FILE_SIZE_MIB)
                .ok_or("Enter an input file size limit from 0 to 1024 MiB.")?;
        }
        options.parallelism = (!self.parallelism.trim().is_empty())
            .then(|| self.parallelism.parse())
            .transpose()
            .map_err(|_| "Enter a whole number for parallelism.")?;
        options.timeout_seconds = self
            .timeout
            .parse()
            .map_err(|_| "Enter the timeout in whole seconds.")?;
        options.retries = self
            .retries
            .parse()
            .map_err(|_| "Enter a whole number for retries.")?;
        options.jump_host = (!self.jump.trim().is_empty())
            .then(|| parse_login(&self.jump))
            .transpose()?;
        Request {
            language: self.language,
            hosts,
            options,
            hardening,
            goal,
            instructions: self.instructions.clone(),
        }
        .input(mask)
    }

    fn review(
        &mut self,
        profile_display: String,
        authority: Authority,
        system: String,
        input: String,
        header: Header,
    ) -> Result<(), String> {
        let cli = CliPresentation::prepare(&authority.transport, &system, &input)?.map(Box::new);
        self.epoch += 1;
        self.stage = Stage::Review {
            profile_display,
            authority,
            system,
            input,
            cli,
            header,
        };
        Ok(())
    }

    pub(super) fn reviewed(&self, epoch: u64) -> Option<(&Authority, &str, &Header)> {
        match &self.stage {
            Stage::Review {
                authority,
                input,
                header,
                ..
            } if epoch == self.epoch => Some((authority, input, header)),
            _ => None,
        }
    }

    fn generate(
        &mut self,
        id: window::Id,
        epoch: u64,
        config: &Configuration,
        workspace: &Workspace,
    ) -> Result<Task<Desktop>, String> {
        let Stage::Review {
            authority,
            system,
            input,
            header,
            ..
        } = &self.stage
        else {
            return Err("The workflow review is no longer current.".into());
        };
        if epoch != self.epoch {
            return Err("The workflow review is no longer current.".into());
        }
        authority.require_current(config, workspace)?;
        let dispatch = authority.clone();
        let pending = engine::generate(
            authority.transport.clone(),
            system.clone(),
            input.into(),
            self.language,
            header.clone(),
            move || dispatch.require_dispatch(),
        )?;
        self.stage = Stage::Waiting {
            _cancel: pending.cancel,
            authority: authority.clone(),
        };
        let generation = self.generation;
        Ok(Task::perform(
            async move {
                Arc::new(
                    pending
                        .reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The workflow generator stopped.".into())),
                )
            },
            move |result| {
                Desktop::TerminalAgent(
                    id,
                    RunMessage::Swarm(GroupMessage::Workflow(
                        generation,
                        Message::Reply(epoch, result),
                    )),
                )
            },
        ))
    }

    fn waiting(&self, epoch: u64) -> Option<&Authority> {
        match &self.stage {
            Stage::Waiting { authority, .. } if epoch == self.epoch => Some(authority),
            _ => None,
        }
    }

    pub(super) fn update(&mut self, message: Message) -> Result<(), String> {
        match message {
            Message::Cancel | Message::Back => {
                self.epoch += 1;
                self.stage = Stage::Setup;
            }
            Message::Reply(epoch, result) if epoch == self.epoch && self.running() => {
                self.stage = Stage::Setup;
                let draft = result.as_ref().as_ref().map_err(Clone::clone)?;
                self.stage = Stage::Generated(draft.clone());
            }
            _ if !matches!(self.stage, Stage::Setup) => {}
            Message::Language(value) => self.language = value,
            Message::HostSource(value) => self.host_source = value,
            Message::Hosts(action) => super::edit_bounded(&mut self.hosts, action, 8192),
            Message::Inventory(value) if value.len() <= 4096 => self.inventory = value,
            Message::Instructions(value) if value.len() <= 4096 => self.instructions = value,
            Message::Parallelism(value) if value.len() <= 3 => self.parallelism = value,
            Message::Timeout(value) if value.len() <= 4 => self.timeout = value,
            Message::Retries(value) if value.len() <= 2 => self.retries = value,
            Message::Jump(value) if value.len() <= 512 => self.jump = value,
            Message::FileSize(value) if value.len() <= 4 => self.file_size = value,
            Message::Toggle(flag, checked) => {
                *match flag {
                    Flag::ContinueOnError => &mut self.options.continue_on_error,
                    Flag::CollectResults => &mut self.options.collect_results,
                    Flag::Aggregate => &mut self.options.aggregate_report,
                    Flag::Strict => &mut self.options.strict_ssh,
                    Flag::DryRun => &mut self.options.dry_run,
                    Flag::Sudo => &mut self.options.sudo,
                    Flag::InputHardening => &mut self.hardening.input_enabled,
                } = checked
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        match &self.stage {
            Stage::Review {
                profile_display,
                authority,
                system,
                input,
                cli,
                header,
                ..
            } => column![
                text("Review workflow generation").size(20),
                text(profile_display),
                scrollable(
                    column![
                        if let Some(cli) = cli {
                            cli.view()
                        } else {
                            column![
                                text("System instructions"),
                                text(system).font(iced::Font::MONOSPACE),
                                text("Model input"),
                                text(input).font(iced::Font::MONOSPACE),
                            ]
                            .spacing(6)
                            .into()
                        },
                        text("Script header added locally"),
                        text(header.text().unwrap_or_default()).font(iced::Font::MONOSPACE),
                    ]
                    .spacing(6)
                )
                .height(Length::Fill),
                row![
                    button(if cli.is_some() {
                        "Run reviewed CLI request"
                    } else if authority.outbound.is_some() {
                        "Send reviewed HTTPS request"
                    } else {
                        "Generate draft"
                    })
                    .on_press(Message::Generate(self.epoch)),
                    button("Back").on_press(Message::Back)
                ]
                .spacing(6),
            ]
            .spacing(8)
            .into(),
            Stage::Waiting { .. } => column![
                text("Generating a workflow draft…"),
                button("Cancel").on_press(Message::Cancel)
            ]
            .spacing(8)
            .into(),
            Stage::Generated(draft) => column![
                text("Generated workflow").size(20),
                text(draft.name()),
                scrollable(text(draft.code()).font(iced::Font::MONOSPACE)).height(Length::Fill),
                row![
                    button("Open unsaved snippet").on_press(Message::OpenSnippet(self.epoch)),
                    button("Back").on_press(Message::Back)
                ]
                .spacing(6),
            ]
            .spacing(8)
            .into(),
            Stage::Setup => {
                let mut form = column![
                    text("Generate a workflow draft").size(20),
                    pick_list(Language::ALL, Some(self.language), Message::Language),
                    pick_list(
                        [
                            HostSource::Selected,
                            HostSource::Manual,
                            HostSource::Inventory
                        ],
                        Some(self.host_source),
                        Message::HostSource
                    ),
                ]
                .spacing(8);
                form = match self.host_source {
                    HostSource::Selected => {
                        form.push(text("Uses the SSH panes selected in the swarm."))
                    }
                    HostSource::Manual => form.push(
                        text_editor(&self.hosts)
                            .placeholder("user@host[:port], one per line")
                            .height(76)
                            .on_action(Message::Hosts),
                    ),
                    HostSource::Inventory => form.push(
                        text_input("Absolute inventory path", &self.inventory)
                            .on_input(Message::Inventory),
                    ),
                };
                let toggle = |label, checked, flag| {
                    checkbox(checked)
                        .label(label)
                        .on_toggle(move |value| Message::Toggle(flag, value))
                };
                form = form
                    .push(
                        text_input("Additional instructions", &self.instructions)
                            .on_input(Message::Instructions),
                    )
                    .push(
                        row![
                            text_input("Parallelism", &self.parallelism)
                                .on_input(Message::Parallelism),
                            text_input("Timeout (seconds)", &self.timeout)
                                .on_input(Message::Timeout),
                            text_input("Retries", &self.retries).on_input(Message::Retries),
                        ]
                        .spacing(6),
                    )
                    .push(
                        text_input("Optional jump host (user@host[:port])", &self.jump)
                            .on_input(Message::Jump),
                    )
                    .push(
                        row![
                            toggle(
                                "Continue on error",
                                self.options.continue_on_error,
                                Flag::ContinueOnError
                            ),
                            toggle(
                                "Collect results",
                                self.options.collect_results,
                                Flag::CollectResults
                            ),
                            toggle(
                                "Aggregate report",
                                self.options.aggregate_report,
                                Flag::Aggregate
                            ),
                            toggle("Strict SSH checks", self.options.strict_ssh, Flag::Strict),
                            toggle("Dry run", self.options.dry_run, Flag::DryRun),
                            toggle("Use sudo", self.options.sudo, Flag::Sudo),
                        ]
                        .spacing(6)
                        .wrap(),
                    );
                if self.language != Language::Ansible {
                    form = form.push(toggle(
                        "Add input validation guards",
                        self.hardening.input_enabled,
                        Flag::InputHardening,
                    ));
                    if self.hardening.input_enabled {
                        form = form.push(
                            text_input("Input file size in MiB (0 is unlimited)", &self.file_size)
                                .on_input(Message::FileSize),
                        );
                    }
                }
                form.push(button("Review model input").on_press(Message::Review))
                    .into()
            }
        }
    }
}

impl Workspace {
    pub(super) fn swarm_workflow_action(
        &mut self,
        id: window::Id,
        group: &mut Group,
        message: Message,
    ) -> Task<Desktop> {
        let Some(mut form) = group.workflow.take() else {
            return Task::none();
        };
        let result = self.swarm_workflow_transition(id, group, &mut form, message);
        group.workflow = Some(form);
        match result {
            Ok(task) => task,
            Err(error) => {
                group.notice = error;
                Task::none()
            }
        }
    }

    fn swarm_workflow_transition(
        &mut self,
        id: window::Id,
        group: &Group,
        form: &mut Form,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        match message {
            Message::Review if matches!(form.stage, Stage::Setup) => {
                self.swarm_ai_policy(super::Mode::Ask)?;
                let config = self
                    .ai_configuration()
                    .ok_or("AI is unavailable under the current settings.")?;
                let profile = group
                    .profile
                    .as_ref()
                    .ok_or("Choose an available AI profile.")?;
                let (_, name, model, destination) = swarm_profile_entries(&config)
                    .find(|(id, ..)| *id == profile.id)
                    .ok_or("The selected workflow profile is unavailable.")?;
                let (transport, limit) = profile.transport(&config)?;
                let outbound = OutboundAuthority::capture(profile, &transport, self.vault.as_ref());
                let authority = Authority {
                    configuration: Arc::clone(&config),
                    profile: profile.clone(),
                    transport,
                    limit,
                    redactions: self.clipboard.policy.terminal_logging.clone(),
                    outbound,
                };
                let (input, redactors) = self.swarm_workflow_input(group, form, &authority)?;
                let goal = engine::reviewed_goal(&input)?;
                let creator = kortty_pty::local_username().unwrap_or_else(|| "unknown".into());
                let mask = |text: &str| authority.mask_text(&redactors, text, self);
                let name = mask(name)?;
                let header = Header::automatic(
                    form.language,
                    HeaderFacts {
                        goal: &goal,
                        creator: &mask(&creator)?,
                        ssh_target: None,
                        generated_at: jiff::Zoned::now().datetime(),
                        profile_name: &name,
                    },
                );
                check_header(&redactors, &header)?;
                let system = engine::system(form.language, &header);
                let system = mask(&authority.transport.render_system_prompt(&system))?;
                if system.chars().count() + input.chars().count() > limit {
                    return Err(
                        "The workflow request exceeds the selected profile's context limit.".into(),
                    );
                }
                let profile_display = if authority.outbound.is_some() {
                    format!("{name} · {} · {}", mask(model)?, mask(&destination)?)
                } else {
                    format!("{name} · {}", mask(model)?)
                };
                authority.require_dispatch()?;
                form.review(profile_display, authority, system, input, header)?;
            }
            Message::Generate(epoch) => {
                self.swarm_ai_policy(super::Mode::Ask)?;
                let Some((authority, reviewed, header)) = form.reviewed(epoch) else {
                    return Ok(Task::none());
                };
                let (current, redactors) = self.swarm_workflow_input(group, form, authority)?;
                if current != reviewed {
                    return Err("The workflow input changed. Review it again.".into());
                }
                check_header(&redactors, header)?;
                let config = self
                    .ai_configuration()
                    .ok_or("AI is unavailable under the current settings.")?;
                return form.generate(id, epoch, &config, self);
            }
            Message::Reply(epoch, result) => {
                let Some(authority) = form.waiting(epoch) else {
                    return Ok(Task::none());
                };
                let checked = (|| {
                    self.swarm_ai_policy(super::Mode::Ask)?;
                    let config = self
                        .ai_configuration()
                        .ok_or("AI is unavailable under the current settings.")?;
                    authority.require_current(&config, self)?;
                    let draft = result.as_ref().as_ref().map_err(Clone::clone)?;
                    self.check_swarm_workflow_draft(group, draft, Some(authority))?;
                    Ok(draft.clone())
                })();
                form.update(Message::Reply(epoch, Arc::new(checked)))?;
            }
            Message::OpenSnippet(epoch) => {
                if let Some(draft) = form.draft(epoch) {
                    self.swarm_ai_policy(super::Mode::Ask)?;
                    self.check_swarm_workflow_draft(group, draft, None)?;
                    return Ok(self.open_workflow_snippet(
                        id,
                        draft.clone().snippet(form.language),
                        draft.goal(),
                        None,
                        Vec::new(),
                    ));
                }
            }
            message => form.update(message)?,
        }
        Ok(Task::none())
    }

    fn check_swarm_workflow_draft(
        &self,
        group: &Group,
        draft: &Draft,
        authority: Option<&Authority>,
    ) -> Result<(), String> {
        let (_, redactors) = self.swarm_workflow_context(group)?;
        for value in [draft.name(), draft.code(), draft.goal()] {
            let masked = match authority {
                Some(authority) => authority.mask_text(&redactors, value, self)?,
                None => mask_text(&redactors, value)?,
            };
            if masked != value {
                return Err("The generated workflow contains text that requires masking.".into());
            }
        }
        Ok(())
    }

    fn swarm_workflow_input(
        &self,
        group: &Group,
        form: &Form,
        authority: &Authority,
    ) -> Result<(String, Vec<Redactor>), String> {
        let (targets, redactors) = self.swarm_workflow_context(group)?;
        let input = form.request(group.goal.clone(), targets, |text| {
            authority.mask_text(&redactors, text, self)
        })?;
        Ok((input, redactors))
    }

    fn swarm_workflow_context(
        &self,
        group: &Group,
    ) -> Result<(Vec<LoginTarget>, Vec<Redactor>), String> {
        let policy = self.swarm_policy_for(SwarmMode::Advisory)?;
        let managed = Redactor::for_journal(
            Secrets::default()
                .seal()
                .map_err(|error| error.to_string())?,
            self.clipboard.policy.terminal_logging.clone(),
        );
        let mut redactors = vec![managed];
        let mut targets = Vec::new();
        for choice in &group.choices {
            let key = choice.target.key;
            if !group.selected.contains(&key) {
                continue;
            }
            if let Some(source) = &choice.saved {
                let current = self.saved_swarm_connection(source, super::Mode::Ask, true)?;
                let (_, redactor) = self.saved_swarm_credentials(source, &current)?;
                redactors.push(redactor);
                targets.push(source.reference.target.clone());
                continue;
            }
            let (session, stream) = key.pane().ok_or("A selected terminal is unavailable.")?;
            let (_, pane, tab) = self
                .agent_pane(session)
                .ok_or("A selected source terminal closed.")?;
            if tab.incognito || pane.stream_generation != stream {
                return Err("A selected terminal changed. Review the workflow again.".into());
            }
            self.agent_policy(pane, super::Mode::Ask)?;
            if pane
                .ssh
                .as_ref()
                .is_some_and(|ssh| !ssh.workflow_route_allowed(&policy))
            {
                return Err("Managed policy blocks a selected SSH route.".into());
            }
            redactors.push(self.agent_source_redactor(pane)?);
            if let Some(ssh) = &pane.ssh {
                targets.push(LoginTarget {
                    host: ssh.host().into(),
                    port: ssh.port(),
                    username: ssh.username().into(),
                });
            }
        }
        Ok((targets, redactors))
    }
}

fn check_header(redactors: &[Redactor], header: &Header) -> Result<(), String> {
    if let Some(text) = header.text()
        && mask_text(redactors, text)? != text
    {
        return Err("The workflow header contains text that requires masking.".into());
    }
    Ok(())
}

fn mask_text(redactors: &[Redactor], text: &str) -> Result<String, String> {
    let mut masked = zeroize::Zeroizing::new(text.to_owned());
    for redactor in redactors {
        *masked = mask::prompt_counted(redactor, &masked)?.0;
    }
    Ok(std::mem::take(&mut *masked))
}

fn parse_login(value: &str) -> Result<LoginTarget, &'static str> {
    const ERROR: &str = "Enter each SSH destination as user@host or user@host:port.";
    let (username, address) = value.trim().rsplit_once('@').ok_or(ERROR)?;
    if username.contains([':', '@']) {
        return Err(ERROR);
    }
    let (host, port) = if let Some(address) = address.strip_prefix('[') {
        let (host, suffix) = address.split_once(']').ok_or(ERROR)?;
        let port = if suffix.is_empty() {
            22
        } else {
            suffix
                .strip_prefix(':')
                .ok_or(ERROR)?
                .parse()
                .map_err(|_| ERROR)?
        };
        (host, port)
    } else if address.matches(':').count() == 1 {
        let (host, port) = address.rsplit_once(':').ok_or(ERROR)?;
        (host, port.parse().map_err(|_| ERROR)?)
    } else {
        (address, 22)
    };
    let login = LoginTarget {
        host: host.into(),
        port,
        username: username.into(),
    };
    login.validate().map_err(|_| ERROR)?;
    Ok(login)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_destinations_allow_ipv6_and_reject_credentials_and_invalid_ports() {
        assert_eq!(parse_login("user@[::1]:2200").unwrap().port, 2200);
        assert_eq!(parse_login("user@host.example").unwrap().port, 22);
        assert!(parse_login("user:password@host.example").is_err());
        assert!(parse_login("user@host.example:0").is_err());
    }
}
