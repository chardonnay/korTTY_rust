//! A separate generation review. Generated code is returned as an unsaved snippet.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length, Task, window};

use crate::ai_manager::{Configuration, reviewed_memory::Memory};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai;
use crate::snippet_analysis::model::hardening::{MAX_FILE_SIZE_MIB, Preferences};
use crate::ssh_dialog::LoginTarget;
use crate::terminal_agent::swarm::workflow::{
    self as engine, Draft, Header, HeaderFacts, Hosts, Language, Options, Request,
};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::workspace::question_history::{History, Kind, MAX_WORKFLOW_INSTRUCTIONS};

use super::super::knowledge_review;
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
    Instructions(text_editor::Action),
    RecallInstructions(String),
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
    instructions: text_editor::Content,
    history: Option<(PathBuf, History)>,
    history_error: Option<String>,
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
        authority: Arc<Authority>,
        system: String,
        input: String,
        cli: Option<Box<CliPresentation>>,
        header: Header,
    },
    Waiting {
        _cancel: local_ai::Cancel,
        authority: Arc<Authority>,
    },
    Generated(Draft),
}

pub(super) struct Authority {
    configuration: Arc<Configuration>,
    profile: ProfileChoice,
    transport: local_ai::Profile,
    limit: usize,
    redactions: Policy,
    outbound: Option<OutboundAuthority>,
    memory: Memory,
    memory_mask: knowledge_review::Mask,
    vault_revision: Weak<()>,
    credential: Option<zeroize::Zeroizing<String>>,
    sources: Sources,
}

#[derive(Default)]
struct Sources {
    saved: Vec<super::headless::Source>,
    live: Vec<LiveSource>,
}

struct LiveSource {
    route: crate::workspace::ssh_access::SshTarget,
    stream: crate::session::StreamLiveness,
}

struct Context {
    targets: Vec<LoginTarget>,
    redactors: Vec<Redactor>,
    sources: Sources,
}

impl Sources {
    fn require_current(&self, policy: &ClipboardPolicy) -> Result<(), String> {
        if self
            .saved
            .iter()
            .any(|source| !source.route_allowed(policy) || !source.revision_current())
            || self.live.iter().any(|source| {
                !source.route.workflow_route_allowed(policy) || !source.stream.running()
            })
        {
            return Err("A selected SSH source changed. Review the workflow again.".into());
        }
        Ok(())
    }
}

impl Authority {
    fn require_current(
        &self,
        live: &Configuration,
        workspace: &Workspace,
        redactors: &[Redactor],
    ) -> Result<(), String> {
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
        let mask = memory_mask(redactors, self.credential.clone(), workspace)?;
        self.memory
            .require_current_with(live, &workspace.clipboard.policy.ai_memory, |text| {
                mask.apply(text)
            })?;
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
        self.sources.require_current(&policy)?;
        if self.vault_revision.strong_count() == 0
            || self.transport.completion_secret() != self.credential
        {
            return Err("The workflow credential mask changed. Review the request again.".into());
        }
        let changed =
            "The saved workflow profile or redaction rules changed. Review the request again.";
        let saved = self.configuration.reload_saved().map_err(|_| changed)?;
        if !self
            .redactions
            .equivalent_redactions(&policy.terminal_logging)
            || !self.matches_profile(&saved)
        {
            return Err(changed.into());
        }
        self.memory
            .require_current_with(&saved, &policy.ai_memory, |text| {
                self.memory_mask.apply(text)
            })
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
            instructions: text_editor::Content::new(),
            history: None,
            history_error: None,
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
    pub(super) fn new(generation: u64, history_root: Option<&Path>) -> Self {
        Self {
            generation,
            history: history_root.and_then(|root| {
                History::load_at(root, Kind::Workflow)
                    .ok()
                    .map(|history| (root.to_owned(), history))
            }),
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
            instructions: self.instructions.text().trim().to_owned(),
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
            authority: Arc::new(authority),
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
            } if epoch == self.epoch => Some((authority.as_ref(), input, header)),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn reviewed_system(&self) -> &str {
        let Stage::Review { system, .. } = &self.stage else {
            panic!("expected a reviewed workflow request");
        };
        system
    }

    fn generate(
        &mut self,
        id: window::Id,
        epoch: u64,
        config: &Configuration,
        workspace: &Workspace,
        redactors: &[Redactor],
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
        authority.require_current(config, workspace, redactors)?;
        let dispatch = Arc::clone(authority);
        let pending = engine::generate(
            authority.transport.clone(),
            system.clone(),
            input.into(),
            self.language,
            header.clone(),
            move || dispatch.require_dispatch(),
        )?;
        if let Some((root, history)) = &mut self.history
            && let Ok(input) = serde_json::from_str::<serde_json::Value>(input)
            && let Some(instructions) = input["instructions"]
                .as_str()
                .filter(|value| !value.is_empty())
        {
            let recorded = History::load_at(root, Kind::Workflow).and_then(|mut current| {
                current.update_at(root, |history| history.record(instructions.to_owned()))?;
                Ok(current)
            });
            match recorded {
                Ok(current) => {
                    *history = current;
                    self.history_error = None;
                }
                Err(error) => {
                    self.history_error =
                        Some(format!("Could not save instruction history ({error})."))
                }
            }
        }
        self.stage = Stage::Waiting {
            _cancel: pending.cancel,
            authority: Arc::clone(authority),
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
            Stage::Waiting { authority, .. } if epoch == self.epoch => Some(authority.as_ref()),
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
            Message::Instructions(action) => {
                super::edit_bounded(&mut self.instructions, action, MAX_WORKFLOW_INSTRUCTIONS);
            }
            Message::RecallInstructions(value)
                if self
                    .history
                    .as_ref()
                    .is_some_and(|(_, history)| history.entries().contains(&value)) =>
            {
                self.instructions = text_editor::Content::with_text(&value);
            }
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
                        text_editor(&self.instructions)
                            .placeholder("Additional instructions")
                            .height(80)
                            .on_action(Message::Instructions),
                    )
                    .push(
                        self.history
                            .as_ref()
                            .filter(|(_, history)| !history.entries().is_empty())
                            .map(|(_, history)| {
                                pick_list(
                                    history.entries(),
                                    None::<String>,
                                    Message::RecallInstructions,
                                )
                                .placeholder("Instruction history")
                                .width(Length::Fill)
                            }),
                    )
                    .push(
                        self.history_error
                            .as_ref()
                            .map(|error| text(error).size(13)),
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
                let Context {
                    targets,
                    redactors,
                    sources,
                } = self.swarm_workflow_context(group)?;
                let credential = transport.completion_secret();
                let memory_mask = memory_mask(&redactors, credential.clone(), self)?;
                let memory = Memory::masked(
                    &config,
                    &self.clipboard.policy.ai_memory,
                    None,
                    true,
                    |text| memory_mask.apply(text),
                )?;
                let authority = Authority {
                    configuration: Arc::clone(&config),
                    profile: profile.clone(),
                    transport,
                    limit,
                    redactions: self.clipboard.policy.terminal_logging.clone(),
                    outbound,
                    memory,
                    memory_mask,
                    vault_revision: self.vault_change_revision(),
                    credential,
                    sources,
                };
                let mask = |text: &str| authority.mask_text(&redactors, text, self);
                let input = form.request(group.goal.clone(), targets, mask)?;
                let goal = engine::reviewed_goal(&input)?;
                let creator = kortty_pty::local_username().unwrap_or_else(|| "unknown".into());
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
                let system = authority.memory.system(&system, None);
                let system = mask(&authority.transport.render_system_prompt(&system))?;
                if authority.memory.text().is_some()
                    && authority.memory_mask.apply(&system)? != system
                {
                    return Err("The workflow AI memory could not be safely included.".into());
                }
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
                let Context {
                    targets, redactors, ..
                } = self.swarm_workflow_context(group)?;
                let current = form.request(group.goal.clone(), targets, |text| {
                    authority.mask_text(&redactors, text, self)
                })?;
                if current != reviewed {
                    return Err("The workflow input changed. Review it again.".into());
                }
                check_header(&redactors, header)?;
                let config = self
                    .ai_configuration()
                    .ok_or("AI is unavailable under the current settings.")?;
                return form.generate(id, epoch, &config, self, &redactors);
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
                    let Context { redactors, .. } = self.swarm_workflow_context(group)?;
                    authority.require_current(&config, self, &redactors)?;
                    let draft = result.as_ref().as_ref().map_err(Clone::clone)?;
                    self.check_swarm_workflow_draft(&redactors, draft, Some(authority))?;
                    Ok(draft.clone())
                })();
                form.update(Message::Reply(epoch, Arc::new(checked)))?;
            }
            Message::OpenSnippet(epoch) => {
                if let Some(draft) = form.draft(epoch) {
                    self.swarm_ai_policy(super::Mode::Ask)?;
                    let Context { redactors, .. } = self.swarm_workflow_context(group)?;
                    self.check_swarm_workflow_draft(&redactors, draft, None)?;
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
        redactors: &[Redactor],
        draft: &Draft,
        authority: Option<&Authority>,
    ) -> Result<(), String> {
        for value in [draft.name(), draft.code(), draft.goal()] {
            let masked = match authority {
                Some(authority) => authority.mask_text(redactors, value, self)?,
                None => mask_text(redactors, value)?,
            };
            if masked != value {
                return Err("The generated workflow contains text that requires masking.".into());
            }
        }
        Ok(())
    }

    fn swarm_workflow_context(&self, group: &Group) -> Result<Context, String> {
        let policy = self.swarm_policy_for(SwarmMode::Advisory)?;
        let managed = Redactor::for_journal(
            Secrets::default()
                .seal()
                .map_err(|error| error.to_string())?,
            self.clipboard.policy.terminal_logging.clone(),
        );
        let mut redactors = vec![managed];
        let mut targets = Vec::new();
        let mut sources = Sources::default();
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
                sources.saved.push(source.clone());
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
                let stream = pane
                    .session
                    .stream_liveness()
                    .ok_or("Could not verify the workflow credential mask.")?;
                sources.live.push(LiveSource {
                    route: ssh.clone(),
                    stream,
                });
                targets.push(LoginTarget {
                    host: ssh.host().into(),
                    port: ssh.port(),
                    username: ssh.username().into(),
                });
            }
        }
        Ok(Context {
            targets,
            redactors,
            sources,
        })
    }
}

fn memory_mask(
    redactors: &[Redactor],
    credential: Option<zeroize::Zeroizing<String>>,
    workspace: &Workspace,
) -> Result<knowledge_review::Mask, String> {
    let count = redactors.len().saturating_sub(1);
    let secrets = Secrets::scheduled_targets(count);
    for redactor in redactors {
        redactor.add_known_secrets(&secrets);
    }
    let secrets = secrets
        .seal()
        .map_err(|_| "Could not verify the workflow credential mask.")?;
    let redactor =
        Redactor::for_journal(secrets, workspace.clipboard.policy.terminal_logging.clone());
    Ok(
        knowledge_review::Mask::new(redactor, credential, workspace.vault.as_ref())
            .for_targets(count),
    )
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
