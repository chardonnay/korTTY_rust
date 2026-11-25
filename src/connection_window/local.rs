use std::path::PathBuf;

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::{Element, Length, Task};
use kortty_pty::{LocalShell, available_local_shells};
use kortty_terminal::TerminalProfile;

use crate::connection_store::LocalProfile;
use crate::session_security::{IsolationLevel, LocalIsolation};
use crate::terminal_effects::{Effect, editor::EffectDraft};

use super::{ConnectionWindow, Message, Persisted, Scope, terminal_overrides};

const MAX_ARGUMENTS: usize = 127;

#[derive(Clone, Copy, PartialEq)]
struct ShellChoice(Option<LocalShell>);

impl std::fmt::Display for ShellChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(shell) => shell.fmt(f),
            None => f.write_str("Custom command"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Focus(iced::widget::Id),
    Name(String),
    Directory(String),
    Folder(Option<String>),
    Shell(LocalShell),
    SelectShell(Option<LocalShell>),
    Emulation(TerminalProfile),
    Isolation(LocalIsolation),
    Integration(bool),
    Custom(bool),
    Program(String),
    AddArgument,
    Argument(usize, String),
    RemoveArgument(usize),
    Terminal(terminal_overrides::Edit),
    Effect(Option<Effect>),
    EffectSpeed(String),
}

pub(super) struct Draft {
    pub(super) id: u64,
    pub(super) changed: bool,
    name: String,
    directory: String,
    shell: LocalShell,
    shell_picker: iced::widget::Id,
    emulation: TerminalProfile,
    isolation: LocalIsolation,
    integration: bool,
    custom: bool,
    program: String,
    program_field: iced::widget::Id,
    arguments: Vec<String>,
    argument_fields: Vec<iced::widget::Id>,
    terminal: terminal_overrides::Draft,
    effect: EffectDraft,
}

impl Draft {
    pub(super) fn protocol_isolation(&self) -> LocalIsolation {
        self.isolation
    }

    pub(super) fn set_protocol_isolation(&mut self, value: Option<IsolationLevel>) {
        self.isolation = LocalIsolation::from_override(value);
    }
    pub(super) fn new(profile: Option<LocalProfile>, duplicate: bool) -> Self {
        let mut draft = Self {
            id: 0,
            changed: false,
            name: String::new(),
            directory: String::new(),
            shell: LocalShell::default(),
            shell_picker: iced::widget::Id::unique(),
            emulation: TerminalProfile::default(),
            isolation: LocalIsolation::Inherit,
            integration: false,
            custom: false,
            program: String::new(),
            program_field: iced::widget::Id::unique(),
            arguments: Vec::new(),
            argument_fields: Vec::new(),
            terminal: terminal_overrides::Draft::new(&Default::default()),
            effect: EffectDraft::default(),
        };
        if let Some(profile) = profile {
            draft.id = if duplicate { 0 } else { profile.id };
            draft.emulation = profile.profile().unwrap_or_default();
            draft.name = profile.name;
            if duplicate && draft.name.len() <= 153 {
                draft.name.push_str(" (copy)");
            }
            draft.directory = profile
                .working_directory
                .map_or_else(String::new, |directory| {
                    directory.to_string_lossy().into_owned()
                });
            draft.shell = profile.shell;
            draft.isolation = profile.isolation;
            draft.integration = profile.shell_integration;
            draft.terminal = terminal_overrides::Draft::new(&profile.terminal);
            draft.effect = EffectDraft::new(profile.terminal_effect);
            if let Some(command) = profile.command {
                let mut words = command.into_iter();
                draft.custom = true;
                draft.program = words.next().unwrap_or_default();
                draft.arguments = words.collect();
                draft.argument_fields = draft
                    .arguments
                    .iter()
                    .map(|_| iced::widget::Id::unique())
                    .collect();
            }
        }
        draft
    }

    pub(super) fn profile(&self) -> Result<LocalProfile, String> {
        self.profile_named(self.id, &self.name, self.emulation)
    }

    pub(super) fn profile_named(
        &self,
        id: u64,
        name: &str,
        emulation: TerminalProfile,
    ) -> Result<LocalProfile, String> {
        let command = self.custom.then(|| {
            std::iter::once(self.program.clone())
                .chain(self.arguments.iter().cloned())
                .collect()
        });
        let profile = LocalProfile {
            id,
            isolation: self.isolation,
            name: name.trim().into(),
            shell: if self.custom {
                LocalShell::Default
            } else {
                self.shell
            },
            command,
            working_directory: (!self.directory.is_empty()).then(|| PathBuf::from(&self.directory)),
            shell_integration: self.integration,
            emulation: emulation.term_name().into(),
            terminal_effect: self.effect.selection(),
            terminal: self.terminal.value()?,
        };
        profile.validate().map_err(|error| error.to_string())?;
        Ok(profile)
    }

    pub(super) fn protocol_fields(&self, enabled: bool) -> Element<'_, Edit> {
        let mut shells = available_local_shells();
        if !shells.contains(&self.shell) {
            shells.push(self.shell);
        }
        let selected = ShellChoice((!self.custom).then_some(self.shell));
        let shell: Element<'_, Edit> = if enabled {
            pick_list(
                shells
                    .into_iter()
                    .map(|shell| ShellChoice(Some(shell)))
                    .chain(std::iter::once(ShellChoice(None)))
                    .collect::<Vec<_>>(),
                Some(selected),
                |choice| Edit::SelectShell(choice.0),
            )
            .text_size(13)
            .width(160)
            .keyboard_navigation(self.shell_picker.clone())
            .on_open(Edit::Focus(self.shell_picker.clone()))
            .into()
        } else {
            button(row![
                text(selected.to_string()).size(13).width(Length::Fill),
                text("▾").size(13)
            ])
            .style(crate::workspace::dialog_design::secondary_button)
            .width(160)
            .into()
        };
        let mut content = column![
            protocol_row("Shell", shell),
            protocol_row(
                "Custom command",
                text_input("Executable path or name", &self.program)
                    .id(self.program_field.clone())
                    .size(13)
                    .width(430)
                    .on_input_maybe((enabled && self.custom).then_some(Edit::Program))
            ),
        ]
        .spacing(10);
        if self.custom {
            for (index, value) in self.arguments.iter().enumerate() {
                content = content.push(protocol_row(
                    "Argument",
                    row![
                        text_input("Argument", value)
                            .id(self.argument_fields[index].clone())
                            .size(13)
                            .on_input_maybe(
                                enabled.then_some(move |value| Edit::Argument(index, value))
                            ),
                        protocol_button("Remove", enabled.then_some(Edit::RemoveArgument(index))),
                    ]
                    .spacing(8),
                ));
            }
            content = content.push(protocol_row(
                "",
                protocol_button(
                    "Add argument",
                    (enabled && self.arguments.len() < MAX_ARGUMENTS).then_some(Edit::AddArgument),
                ),
            ));
        }
        content
            .push(protocol_row(
                "Start directory",
                text_input("Absolute path (optional)", &self.directory)
                    .size(13)
                    .width(430)
                    .on_input_maybe(enabled.then_some(Edit::Directory)),
            ))
            .push(protocol_row(
                "Shell integration",
                crate::workspace::dialog_design::keyboard_table::action(
                    checkbox(self.integration)
                        .label("Add shell integration automatically")
                        .text_size(13)
                        .on_toggle_maybe(
                            (enabled && (self.integration || self.integration_support().is_ok()))
                                .then_some(Edit::Integration),
                        )
                        .into(),
                    (enabled && (self.integration || self.integration_support().is_ok()))
                        .then_some(Edit::Integration(!self.integration)),
                    false,
                ),
            ))
            .push(protocol_row(
                "",
                text(if enabled {
                    "For supported shells on this computer. Your startup files are not changed."
                } else {
                    "Only for local shell connections."
                })
                .size(12)
                .style(text::secondary),
            ))
            .into()
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Name(name) if name.len() <= 160 => self.name = name,
            Edit::Directory(directory) if directory.len() <= 4096 => self.directory = directory,
            Edit::Shell(shell) if !self.custom => {
                self.shell = shell;
                self.integration &= self.integration_support().is_ok();
            }
            Edit::SelectShell(shell) => {
                self.custom = shell.is_none();
                if let Some(shell) = shell {
                    self.shell = shell;
                }
                self.integration &= self.integration_support().is_ok();
            }
            Edit::Emulation(emulation) => self.emulation = emulation,
            Edit::Isolation(isolation) => self.isolation = isolation,
            Edit::Integration(enabled) if !enabled || self.integration_support().is_ok() => {
                self.integration = enabled;
            }
            Edit::Custom(custom) => self.custom = custom,
            Edit::Program(program) if self.custom && program.len() <= 8192 => {
                self.program = program;
            }
            Edit::AddArgument if self.custom && self.arguments.len() < MAX_ARGUMENTS => {
                self.arguments.push(String::new());
                self.argument_fields.push(iced::widget::Id::unique());
            }
            Edit::Argument(index, argument) if self.custom && argument.len() <= 8192 => {
                let Some(current) = self.arguments.get_mut(index) else {
                    return;
                };
                *current = argument;
            }
            Edit::RemoveArgument(index) if self.custom && index < self.arguments.len() => {
                self.arguments.remove(index);
                self.argument_fields.remove(index);
            }
            Edit::Terminal(edit) => {
                self.changed |= self.terminal.update(edit);
                return;
            }
            Edit::Effect(effect) => self.effect.effect = effect,
            Edit::EffectSpeed(value) => self.effect.set_speed(value),
            _ => return,
        }
        self.changed = true;
    }

    pub(super) fn argument_focus(&self, edit: &Edit) -> Option<iced::widget::Id> {
        if !self.custom {
            return None;
        }
        match edit {
            Edit::AddArgument => self.argument_fields.last().cloned(),
            Edit::RemoveArgument(index) => self
                .argument_fields
                .get(*index)
                .or_else(|| self.argument_fields.last())
                .cloned()
                .or_else(|| Some(self.program_field.clone())),
            _ => None,
        }
    }

    fn integration_support(&self) -> std::io::Result<()> {
        let command = self.custom.then(|| {
            std::iter::once(self.program.clone())
                .chain(self.arguments.iter().cloned())
                .collect::<Vec<_>>()
        });
        kortty_pty::validate_shell_integration(
            if self.custom {
                LocalShell::Default
            } else {
                self.shell
            },
            command.as_deref(),
        )
    }

    pub(super) fn view<'a>(
        &'a self,
        title: &'static str,
        choices: &super::view::ViewChoices<'_>,
        group: Element<'a, Edit>,
        inherited: Option<(&str, IsolationLevel)>,
    ) -> Element<'a, Edit> {
        let (level, source) = self.isolation.resolve(
            choices.local_isolation_default,
            inherited,
            choices.isolation.1,
        );
        let mut form = column![
            text(title).size(21),
            text("Name").size(14),
            text_input("Local shell profile name", &self.name).on_input(Edit::Name),
            group,
            text("Starting directory").size(14),
            text_input("Absolute path (optional)", &self.directory).on_input(Edit::Directory),
            column![
                text("Session isolation").size(16),
                terminal_overrides::isolation_row(
                    "Isolation",
                    pick_list(LocalIsolation::ALL, Some(self.isolation), Edit::Isolation)
                        .width(Length::Fill),
                ),
                text(format!("Effective isolation · {level} · {source}")).size(13),
                self.terminal.strict_mode_view().map(Edit::Terminal),
            ]
            .spacing(6),
            text("Terminal emulation").size(14),
            pick_list(TerminalProfile::ALL, Some(self.emulation), Edit::Emulation)
                .width(Length::Fill),
            checkbox(self.custom)
                .label("Run a custom program")
                .on_toggle(Edit::Custom),
        ]
        .spacing(8);
        if self.custom {
            form = form
                .push(text_input("Executable path or name", &self.program).on_input(Edit::Program));
            for (index, argument) in self.arguments.iter().enumerate() {
                form = form.push(
                    row![
                        text_input(&format!("Argument {}", index + 1), argument)
                            .on_input(move |value| Edit::Argument(index, value)),
                        button("Remove").on_press(Edit::RemoveArgument(index)),
                    ]
                    .spacing(8),
                );
            }
            form = form.push(button("Add argument").on_press_maybe(
                (self.arguments.len() < MAX_ARGUMENTS).then_some(Edit::AddArgument),
            ));
        } else {
            let mut shells = available_local_shells();
            if !shells.contains(&self.shell) {
                shells.push(self.shell);
            }
            form = form.push(pick_list(shells, Some(self.shell), Edit::Shell).width(Length::Fill));
        }
        let integration = self.integration_support();
        form = form.push(
            checkbox(self.integration)
                .label("Enable automatic shell integration")
                .on_toggle_maybe(
                    (self.integration || integration.is_ok()).then_some(Edit::Integration),
                ),
        );
        if let Err(error) = integration {
            form = form.push(text(error.to_string()).size(12));
        }
        if let Some(effects) = choices.terminal_effects {
            form = form
                .push(text("Terminal Effect").size(16))
                .push(self.effect.view(effects, Edit::Effect, Edit::EffectSpeed));
        }
        form.push(self.terminal.view(choices, true).map(Edit::Terminal))
            .into()
    }
}

fn protocol_button(label: &'static str, action: Option<Edit>) -> Element<'static, Edit> {
    crate::workspace::dialog_design::keyboard_table::action(
        button(text(label).size(13))
            .on_press_maybe(action.clone())
            .into(),
        action,
        true,
    )
}

fn protocol_row<'a>(label: &'a str, field: impl Into<Element<'a, Edit>>) -> Element<'a, Edit> {
    row![text(label).size(13).width(148), field.into()]
        .spacing(10)
        .align_y(iced::Alignment::Center)
        .into()
}

impl ConnectionWindow {
    pub(super) fn begin_local(&mut self, profile: Option<LocalProfile>, duplicate: bool) {
        let _ = self.begin_protocol_edit(
            profile.map(|value| crate::connection_store::protocol::Profile::Local(Box::new(value))),
            crate::connection_store::protocol::Kind::Local,
            duplicate,
        );
    }

    pub(super) fn save_local(&mut self) -> Task<Message> {
        let Some(draft) = &self.local_draft else {
            return Task::none();
        };
        let profile = match draft.profile() {
            Ok(profile) => profile,
            Err(error) => {
                self.notice = Some(error);
                return Task::none();
            }
        };
        let group = self.draft_group.clone();
        self.persist(move |mut store| {
            let id = store.save_local_in_group(profile, group.as_deref())?;
            let mut done = Persisted::saved(store, id);
            done.scope = Some(group.map_or(Scope::All, Scope::Group));
            Ok(done)
        })
    }

    pub(super) fn visible_local(&self) -> Vec<&LocalProfile> {
        let mut profiles: Vec<_> = self
            .store
            .search_local(&self.query)
            .filter(|profile| match self.scope {
                Scope::All => true,
                Scope::Favorites => self.store.is_favorite(profile.id),
                Scope::Open => self.open_count(profile.id) > 0,
                Scope::Group(ref path) => self.store.is_in_group(profile.id, path),
                _ => false,
            })
            .collect();
        profiles.sort_by_cached_key(|profile| profile.name.to_lowercase());
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_shell_integration_draft_keeps_opt_in_for_presets_and_supported_custom_shells() {
        for shell in [
            LocalShell::Default,
            LocalShell::PowerShell,
            LocalShell::CommandPrompt,
        ] {
            let mut draft = Draft::new(None, false);
            draft.update(Edit::Name("Integrated shell".into()));
            draft.update(Edit::Shell(shell));
            draft.update(Edit::Integration(true));
            let profile = draft.profile().unwrap();
            assert_eq!(profile.shell, shell);
            assert!(profile.shell_integration);
        }
        let mut draft = Draft::new(None, false);
        draft.update(Edit::Name("Custom integration".into()));
        draft.update(Edit::Custom(true));
        draft.update(Edit::Program(r"C:\Program Files\Git\bin\bash.exe".into()));
        draft.update(Edit::AddArgument);
        draft.update(Edit::Argument(0, "-il".into()));
        draft.update(Edit::Integration(true));
        assert!(draft.profile().unwrap().shell_integration);
        draft.update(Edit::Argument(0, "--norc".into()));
        assert!(draft.profile().is_err());
        assert!(draft.integration);
        draft.update(Edit::Integration(false));
        assert!(!draft.profile().unwrap().shell_integration);
    }
}
