use std::fmt;
use std::path::PathBuf;

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::{Element, Length, Task};
use kortty_pty::LocalShell;
use kortty_terminal::TerminalProfile;

use crate::connection_store::LocalProfile;

use super::{ConnectionWindow, Message, Persisted, Scope, terminal_overrides};

const MAX_ARGUMENTS: usize = 127;

#[derive(Clone, Copy, PartialEq, Eq)]
struct ShellChoice(LocalShell);

impl fmt::Display for ShellChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.preset_title().unwrap_or("System default"))
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Name(String),
    Directory(String),
    Shell(LocalShell),
    Emulation(TerminalProfile),
    Integration(bool),
    Custom(bool),
    Program(String),
    AddArgument,
    Argument(usize, String),
    RemoveArgument(usize),
    Terminal(terminal_overrides::Edit),
}

pub(super) struct Draft {
    pub(super) id: u64,
    pub(super) changed: bool,
    name: String,
    directory: String,
    shell: LocalShell,
    emulation: TerminalProfile,
    integration: bool,
    custom: bool,
    program: String,
    arguments: Vec<String>,
    terminal: terminal_overrides::Draft,
}

impl Draft {
    fn new(profile: Option<LocalProfile>, duplicate: bool) -> Self {
        let mut draft = Self {
            id: 0,
            changed: false,
            name: String::new(),
            directory: String::new(),
            shell: LocalShell::default(),
            emulation: TerminalProfile::default(),
            integration: false,
            custom: false,
            program: String::new(),
            arguments: Vec::new(),
            terminal: terminal_overrides::Draft::new(&Default::default()),
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
            draft.integration = profile.shell_integration;
            draft.terminal = terminal_overrides::Draft::new(&profile.terminal);
            if let Some(command) = profile.command {
                let mut words = command.into_iter();
                draft.custom = true;
                draft.program = words.next().unwrap_or_default();
                draft.arguments = words.collect();
            }
        }
        draft
    }

    pub(super) fn profile(&self) -> Result<LocalProfile, String> {
        let command = self.custom.then(|| {
            std::iter::once(self.program.clone())
                .chain(self.arguments.iter().cloned())
                .collect()
        });
        let profile = LocalProfile {
            id: self.id,
            name: self.name.trim().into(),
            shell: if self.custom {
                LocalShell::Default
            } else {
                self.shell
            },
            command,
            working_directory: (!self.directory.is_empty()).then(|| PathBuf::from(&self.directory)),
            shell_integration: self.integration,
            emulation: self.emulation.term_name().into(),
            terminal: self.terminal.value()?,
        };
        profile.validate().map_err(|error| error.to_string())?;
        Ok(profile)
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Name(name) if name.len() <= 160 => self.name = name,
            Edit::Directory(directory) if directory.len() <= 4096 => self.directory = directory,
            Edit::Shell(shell) if !self.custom => self.shell = shell,
            Edit::Emulation(emulation) => self.emulation = emulation,
            Edit::Integration(enabled) if !enabled || self.integration_support().is_ok() => {
                self.integration = enabled;
            }
            Edit::Custom(custom) => self.custom = custom,
            Edit::Program(program) if self.custom && program.len() <= 8192 => {
                self.program = program;
            }
            Edit::AddArgument if self.custom && self.arguments.len() < MAX_ARGUMENTS => {
                self.arguments.push(String::new());
            }
            Edit::Argument(index, argument) if self.custom && argument.len() <= 8192 => {
                let Some(current) = self.arguments.get_mut(index) else {
                    return;
                };
                *current = argument;
            }
            Edit::RemoveArgument(index) if self.custom && index < self.arguments.len() => {
                self.arguments.remove(index);
            }
            Edit::Terminal(edit) => {
                self.changed |= self.terminal.update(edit);
                return;
            }
            _ => return,
        }
        self.changed = true;
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

    pub(super) fn view(
        &self,
        title: &'static str,
        choices: &super::view::ViewChoices<'_>,
    ) -> Element<'_, Edit> {
        let mut form = column![
            text(title).size(21),
            text("Name").size(14),
            text_input("Local shell profile name", &self.name).on_input(Edit::Name),
            text("Starting directory").size(14),
            text_input("Absolute path (optional)", &self.directory).on_input(Edit::Directory),
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
            let mut shells = vec![ShellChoice(LocalShell::Default)];
            if cfg!(windows) {
                shells.extend([
                    ShellChoice(LocalShell::PowerShell),
                    ShellChoice(LocalShell::CommandPrompt),
                ]);
            } else if !self.shell.is_default() {
                shells.push(ShellChoice(self.shell));
            }
            form = form.push(
                pick_list(
                    shells,
                    Some(ShellChoice(self.shell)),
                    |choice: ShellChoice| Edit::Shell(choice.0),
                )
                .width(Length::Fill),
            );
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
        form.push(self.terminal.view(choices).map(Edit::Terminal))
            .into()
    }
}

impl ConnectionWindow {
    pub(super) fn begin_local(&mut self, profile: Option<LocalProfile>, duplicate: bool) {
        self.local_draft = Some(Draft::new(profile, duplicate));
        self.badge_picker = None;
        self.dismiss_tree_context();
        self.notice = None;
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
        self.persist(move |mut store| {
            let id = store.save_local(profile)?;
            let mut done = Persisted::saved(store, id);
            done.scope = Some(Scope::All);
            Ok(done)
        })
    }

    pub(super) fn visible_local(&self) -> Vec<&LocalProfile> {
        let mut profiles: Vec<_> = self
            .store
            .search_local(&self.query)
            .filter(|profile| match self.scope {
                Scope::All => true,
                Scope::Open => self.open_count(profile.id) > 0,
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
