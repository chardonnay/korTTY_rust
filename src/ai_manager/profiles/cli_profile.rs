//! Program approval and literal argument editing for the configured CLI profile.

use std::path::{Path, PathBuf};

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task};
use serde::{Deserialize, Serialize};

use crate::local_ai::{self, cli};

use super::{Editor, Field, Message, Profile, ProfileId};

mod catalog;
pub(super) use catalog::State as CatalogState;
mod discovery;
pub(super) use discovery::State as DiscoveryState;

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    path: String,
    arguments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<cli::catalog::Provider>,
    pub(super) fingerprint: Option<[u8; 32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimax: Option<MiniMaxSettings>,
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
struct MiniMaxSettings {
    module: String,
    package: Option<String>,
    config: String,
    region: cli::MiniMaxRegion,
}

impl MiniMaxSettings {
    fn new() -> Self {
        let config = std::env::var_os("MMX_CONFIG_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::home_dir().map(|home| home.join(".mmx")))
            .map(|folder| folder.join("config.json"));
        Self {
            config: config
                .and_then(|path| path.into_os_string().into_string().ok())
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    fn valid(&self) -> bool {
        cli::valid_minimax_module_path(Path::new(&self.module))
            && cli::valid_minimax_config_path(Path::new(&self.config))
    }
}

impl Settings {
    pub(super) fn validate_restored(&self, model: &str) -> Result<(), String> {
        if self.fingerprint.is_some()
            || self.path.is_empty()
            || self.path.len() > 4096
            || self.path.chars().any(char::is_control)
            || !cli::valid_arguments(&self.arguments)
            || self.arguments.iter().any(|argument| argument == cli::MODEL)
                && (!local_ai::valid_model(model) || model.starts_with('-'))
            || self.minimax.as_ref().is_some_and(|settings| {
                !settings.valid() || settings.package.is_some() || !self.arguments.is_empty()
            })
        {
            return Err("Invalid unreviewed restored CLI settings.".into());
        }
        Ok(())
    }

    fn has_preset(&self, preset: &[&str]) -> bool {
        self.minimax.is_none()
            && self
                .arguments
                .iter()
                .map(String::as_str)
                .eq(preset.iter().copied())
    }

    pub(super) fn program(&self, model: &str) -> Result<cli::Program, String> {
        let fingerprint = self
            .fingerprint
            .ok_or("Review and approve the CLI executable first.")?;
        if let Some(settings) = &self.minimax {
            if !self.arguments.is_empty() {
                return Err("The MiniMax CLI uses fixed reviewed arguments.".into());
            }
            let package = settings
                .package
                .as_deref()
                .ok_or("Review the official MiniMax CLI package first.")?;
            return cli::Program::approved_minimax(
                self.path.clone().into(),
                fingerprint,
                settings.module.clone().into(),
                package,
                settings.config.clone().into(),
                settings.region,
                model,
            );
        }
        cli::Program::approved(
            self.path.clone().into(),
            &self.arguments,
            model,
            fingerprint,
        )
    }

    pub(super) fn clear_approval(&mut self) {
        self.fingerprint = None;
        if let Some(minimax) = &mut self.minimax {
            minimax.package = None;
        }
    }

    fn inspect(mut self) -> Result<Self, String> {
        let (path, fingerprint) = cli::Program::inspect(Path::new(&self.path))?;
        self.path = path
            .into_os_string()
            .into_string()
            .map_err(|_| "The canonical CLI path is not valid UTF-8.")?;
        self.fingerprint = Some(fingerprint);
        if let Some(minimax) = &mut self.minimax {
            let (module, package) =
                cli::Program::inspect_minimax_module(Path::new(&minimax.module))?;
            minimax.module = module
                .into_os_string()
                .into_string()
                .map_err(|_| "The canonical MiniMax package path is not valid UTF-8.")?;
            minimax.package = Some(package);
        }
        Ok(self)
    }
}

fn preset_arguments(preset: &[&str]) -> Vec<String> {
    preset.iter().map(|argument| (*argument).into()).collect()
}

fn plain_path(value: &str) -> bool {
    value.len() <= 4096 && !value.chars().any(char::is_control)
}

#[derive(Clone)]
pub(crate) enum Action {
    Path(String),
    AddArgument,
    Argument(usize, String),
    RemoveArgument(usize),
    LmStudioPreset { loaded_model: bool },
    CodexPreset,
    MiniMaxPreset,
    LiteralArguments,
    MiniMaxModule(String),
    MiniMaxConfig(String),
    MiniMaxRegion(cli::MiniMaxRegion),
    Inspect,
    Inspected(Result<Box<Settings>, String>),
    Approve,
    Back,
    Catalog(catalog::Action),
    Discovery(discovery::Action),
}

impl Action {
    pub(in crate::ai_manager) fn completed(&self) -> bool {
        matches!(self, Self::Inspected(_))
            || matches!(self, Self::Catalog(action) if action.completed())
            || matches!(self, Self::Discovery(action) if action.completed())
    }
}

#[derive(Default)]
pub(super) struct Review {
    checking: bool,
    draft: Option<(ProfileId, Settings, String)>,
    candidate: Option<Settings>,
}

impl Review {
    fn matches(&self, profile: &Profile) -> bool {
        self.draft.as_ref().is_some_and(|(id, settings, model)| {
            profile.is_cli()
                && profile.id == *id
                && profile.cli.as_ref() == Some(settings)
                && profile.model == *model
        })
    }
}

impl Editor {
    pub(in crate::ai_manager) fn update_cli(
        &mut self,
        action: Action,
        editable: bool,
        discovery_root: Option<&Path>,
    ) -> Task<Message> {
        if let Action::Catalog(action) = action {
            return self.update_cli_catalog(action, editable);
        }
        if let Action::Discovery(action) = action {
            return self.update_cli_discovery(action, editable, discovery_root);
        }
        if let Action::Inspected(result) = action {
            self.cli_review.checking = false;
            if !editable
                || !self
                    .selected()
                    .is_some_and(|profile| self.cli_review.matches(profile))
            {
                self.cli_review = Review::default();
                return Task::none();
            }
            match result {
                Ok(candidate) => {
                    let (_, _, model) = self.cli_review.draft.as_ref().expect("reviewed draft");
                    match candidate.program(model) {
                        Ok(_) => self.cli_review.candidate = Some(*candidate),
                        Err(error) => self.set_status(error),
                    }
                }
                Err(error) => self.set_status(error),
            }
            return Task::none();
        }
        if !editable || !self.selected().is_some_and(Profile::is_cli) {
            return Task::none();
        }
        match action {
            Action::Inspect if !self.cli_review.checking && !self.cli_discovery_busy() => {
                self.cancel_cli_catalog();
                let profile = self.selected().expect("selected CLI profile");
                let settings = profile.cli.clone().unwrap_or_default();
                if !cli::valid_arguments(&settings.arguments)
                    || settings
                        .minimax
                        .as_ref()
                        .is_some_and(|minimax| !minimax.valid())
                    || (!profile.model.is_empty() && !local_ai::valid_model(&profile.model))
                    || (settings.arguments.iter().any(|arg| arg == cli::MODEL)
                        && profile.model.is_empty())
                {
                    self.set_status("Check CLI arguments, selected paths and the model identifier before review.");
                    return Task::none();
                }
                let candidate = settings.clone();
                let (send, receive) = async_channel::bounded(1);
                if std::thread::Builder::new()
                    .name("kortty-cli-executable-review".into())
                    .spawn(move || {
                        let _ = send.try_send(candidate.inspect().map(Box::new));
                    })
                    .is_err()
                {
                    self.set_status("Could not start CLI executable review.");
                    return Task::none();
                }
                self.cli_review = Review {
                    checking: true,
                    draft: Some((profile.id, settings, profile.model.clone())),
                    candidate: None,
                };
                self.set_status("Checking the CLI executable…");
                return Task::perform(
                    async move {
                        receive
                            .recv()
                            .await
                            .unwrap_or_else(|_| Err("CLI executable review stopped.".into()))
                    },
                    |result| Message::Cli(Action::Inspected(result)),
                );
            }
            Action::Approve
                if self
                    .selected()
                    .is_some_and(|profile| self.cli_review.matches(profile)) =>
            {
                if let Some(candidate) = self.cli_review.candidate.take() {
                    let profile = self.selected_mut().expect("selected CLI profile");
                    profile.cli = Some(candidate);
                    profile.restore_review = false;
                    self.cli_review = Review::default();
                    self.set_status("CLI executable approved. Save the profile to use it in reviewed workflows.");
                }
            }
            Action::Back if !self.cli_review.checking => self.cli_review = Review::default(),
            Action::Path(_)
            | Action::AddArgument
            | Action::Argument(..)
            | Action::RemoveArgument(_)
            | Action::LmStudioPreset { .. }
            | Action::CodexPreset
            | Action::MiniMaxPreset
            | Action::LiteralArguments
            | Action::MiniMaxModule(_)
            | Action::MiniMaxConfig(_)
            | Action::MiniMaxRegion(_) => {
                let profile = self.selected_mut().expect("selected CLI profile");
                let settings = profile.cli.get_or_insert_with(Settings::default);
                let mut edited = settings.clone();
                let mut model_cleared = false;
                match action {
                    Action::Path(path)
                        if path.len() <= 4096 && !path.chars().any(char::is_control) =>
                    {
                        edited.path = path
                    }
                    Action::AddArgument
                        if edited.minimax.is_none() && edited.arguments.len() < cli::MAX_ARGS =>
                    {
                        edited.arguments.push(String::new())
                    }
                    Action::Argument(index, value)
                        if edited.minimax.is_none() && value.len() <= cli::MAX_ARG_BYTES =>
                    {
                        if let Some(argument) = edited.arguments.get_mut(index) {
                            *argument = value;
                        }
                    }
                    Action::RemoveArgument(index)
                        if edited.minimax.is_none() && index < edited.arguments.len() =>
                    {
                        edited.arguments.remove(index);
                    }
                    Action::LmStudioPreset { loaded_model } => {
                        if edited
                            .preset_provider()
                            .is_some_and(|provider| provider != cli::catalog::Provider::LmStudioCli)
                        {
                            edited.path.clear();
                        }
                        edited.provider = Some(cli::catalog::Provider::LmStudioCli);
                        let arguments = if loaded_model {
                            model_cleared = !profile.model.is_empty();
                            profile.model.clear();
                            cli::LM_STUDIO_LOADED_STDIN
                        } else {
                            cli::LM_STUDIO_STDIN
                        };
                        edited.arguments = preset_arguments(arguments);
                        edited.minimax = None;
                    }
                    Action::CodexPreset => {
                        if edited
                            .preset_provider()
                            .is_some_and(|provider| provider != cli::catalog::Provider::CodexCli)
                        {
                            edited.path.clear();
                        }
                        edited.provider = Some(cli::catalog::Provider::CodexCli);
                        model_cleared = !profile.model.is_empty();
                        profile.model.clear();
                        edited.arguments = preset_arguments(cli::CODEX_STDIN);
                        edited.minimax = None;
                    }
                    Action::MiniMaxPreset if edited.minimax.is_none() => {
                        edited.provider = Some(cli::catalog::Provider::MiniMax);
                        edited.path.clear();
                        model_cleared = !profile.model.is_empty();
                        profile.model.clear();
                        edited.arguments.clear();
                        edited.minimax = Some(MiniMaxSettings::new());
                    }
                    Action::LiteralArguments => {
                        edited.minimax = None;
                        edited.provider = Some(cli::catalog::Provider::Custom);
                    }
                    Action::MiniMaxModule(path) if plain_path(&path) => {
                        if let Some(minimax) = &mut edited.minimax {
                            minimax.module = path;
                        }
                    }
                    Action::MiniMaxConfig(path) if plain_path(&path) => {
                        if let Some(minimax) = &mut edited.minimax {
                            minimax.config = path;
                        }
                    }
                    Action::MiniMaxRegion(region) => {
                        if let Some(minimax) = &mut edited.minimax {
                            minimax.region = region;
                        }
                    }
                    _ => {}
                }
                if edited != *settings || model_cleared {
                    edited.clear_approval();
                    *settings = edited;
                    self.cli_discovery = None;
                    self.cancel_cli_catalog();
                    if !self.cli_review.checking {
                        self.cli_review = Review::default();
                    }
                    self.status = None;
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn cli_connection<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
    ) -> Element<'a, Message> {
        let Some(settings) = &profile.cli else {
            return text("The CLI profile has no executable settings.").into();
        };
        let enabled = editable && !self.cli_review.checking;
        let mut arguments = column![].spacing(6);
        for (index, argument) in settings.arguments.iter().enumerate() {
            arguments = arguments.push(
                row![
                    text_input("Literal argument", argument).on_input_maybe(
                        enabled
                            .then_some(move |value| Message::Cli(Action::Argument(index, value)))
                    ),
                    button("Remove").style(button::secondary).on_press_maybe(
                        enabled.then_some(Message::Cli(Action::RemoveArgument(index)))
                    ),
                ]
                .spacing(6),
            );
        }
        let review = self
            .cli_review
            .matches(profile)
            .then_some(self.cli_review.candidate.as_ref())
            .flatten()
            .and_then(|candidate| candidate.program(&profile.model).ok());
        let mut content = column![
            self.cli_catalog_view(profile, enabled && review.is_none()),
            text_input(
                if settings.minimax.is_some() {
                    "Absolute Node.js executable path"
                } else {
                    "Absolute native executable path"
                },
                &settings.path
            )
            .on_input_maybe(enabled.then_some(|path| Message::Cli(Action::Path(path)))),
            text_input("Optional model identifier", &profile.model)
                .on_input_maybe(enabled.then_some(|value| Message::Edit(Field::Model, value))),
            row![
                button("LM Studio with model ID")
                    .style(button::secondary)
                    .on_press_maybe(enabled.then_some(Message::Cli(Action::LmStudioPreset {
                        loaded_model: false
                    }))),
                button("LM Studio with loaded model")
                    .style(button::secondary)
                    .on_press_maybe(
                        enabled
                            .then_some(Message::Cli(Action::LmStudioPreset { loaded_model: true }))
                    ),
                button("Codex read-only stdin")
                    .style(button::secondary)
                    .on_press_maybe(enabled.then_some(Message::Cli(Action::CodexPreset))),
                button("MiniMax CLI")
                    .style(button::secondary)
                    .on_press_maybe(enabled.then_some(Message::Cli(Action::MiniMaxPreset))),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(8);
        if let Some(minimax) = &settings.minimax {
            content = content.push(column![
                text("Choose Node.js 22.15 or later and the unmodified dist/mmx.mjs from mmx-cli 1.0.15 or 1.0.29. Sign in with mmx auth login. Refresh expired OAuth sessions with mmx auth refresh.").size(13),
                text_input("Absolute path to dist/mmx.mjs", &minimax.module)
                    .on_input_maybe(enabled.then_some(|path| Message::Cli(Action::MiniMaxModule(path)))),
                text_input("Absolute path to the MiniMax config.json", &minimax.config)
                    .on_input_maybe(enabled.then_some(|path| Message::Cli(Action::MiniMaxConfig(path)))),
                row(cli::MiniMaxRegion::ALL.map(|region| {
                    button(text(region.to_string())).style(if minimax.region == region { button::primary } else { button::secondary })
                        .on_press_maybe((enabled && minimax.region != region).then_some(Message::Cli(Action::MiniMaxRegion(region))))
                        .into()
                })).push(button("Use literal arguments").style(button::secondary)
                    .on_press_maybe(enabled.then_some(Message::Cli(Action::LiteralArguments)))).spacing(8).wrap(),
                text("The reviewed text command sends JSON over stdin to the selected MiniMax HTTPS endpoint. Proxy settings, redirects, and automatic OAuth refresh are disabled.").size(13),
            ].spacing(8));
        } else {
            content = content.push(column![
                text("Both LM Studio presets replace the arguments and require an approved native lms executable. The loaded-model preset clears the model identifier. Load a model in LM Studio before testing. LM Studio selects a loaded model each time the CLI runs.").size(13),
                text("The Codex preset replaces the arguments and clears the model identifier. It requires an approved native Codex executable and an existing Codex sign-in. Review the OpenAI destination before sending each request.").size(13),
                self.cli_discovery_view(profile, enabled && review.is_none()),
                arguments,
                button("Add argument").style(button::secondary).on_press_maybe(
                    (enabled && settings.arguments.len() < cli::MAX_ARGS).then_some(Message::Cli(Action::AddArgument))
                ),
                text("Arguments are literal. Use {model} as one whole argument for the model identifier. Keep secrets out of arguments.").size(13),
                text("Input uses UTF-8 stdin by default. Use one {promptFile} argument for a combined file, or one {systemPromptFile} and one {userPromptFile} for separate roles. Each placeholder occupies one argument.").size(13),
            ].spacing(8));
        }
        content = content
            .push(text("The CLI executable runs with your operating system permissions.").size(13))
            .push(text("Terminal Chat, Agent, Swarm, Scheduler and journal summaries require their own review before using this program.").size(13));
        if let Some(program) = review {
            content = content.push(text(program.command_review()).size(13)).push(
                row![
                    button("Back")
                        .style(button::secondary)
                        .on_press(Message::Cli(Action::Back)),
                    button("Approve CLI executable and arguments")
                        .on_press_maybe(enabled.then_some(Message::Cli(Action::Approve))),
                ]
                .spacing(6),
            );
        } else {
            if let Ok(program) = settings.program(&profile.model) {
                content = content.push(text(program.command_review()).size(13));
            }
            content = content.push(
                button(if self.cli_review.checking {
                    "Checking executable…"
                } else {
                    "Review CLI executable…"
                })
                .on_press_maybe(
                    (enabled && !self.cli_discovery_busy())
                        .then_some(Message::Cli(Action::Inspect)),
                ),
            );
        }
        content.into()
    }
}
