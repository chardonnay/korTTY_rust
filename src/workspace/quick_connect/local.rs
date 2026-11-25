use std::path::PathBuf;

use iced::Element;
use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use kortty_pty::{LocalShell, available_local_shells};

use crate::connection_store::LocalProfile;
use crate::session_security::LocalIsolation;

use super::{Dialog, Message, Workspace, view::labeled};

const MAX_ARGUMENTS: usize = 127;

#[derive(Clone, Debug)]
pub enum Edit {
    SelectShell(Option<LocalShell>),
    Directory(String),
    Isolation(LocalIsolation),
    Program(String),
    AddArgument,
    Argument(usize, String),
    RemoveArgument(usize),
}

pub(super) struct Draft {
    shell_picker: iced::widget::Id,
    program_field: iced::widget::Id,
    argument_fields: Vec<iced::widget::Id>,
    custom: bool,
    directory: String,
    isolation: LocalIsolation,
    program: String,
    arguments: Vec<String>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            shell_picker: iced::widget::Id::unique(),
            program_field: iced::widget::Id::unique(),
            argument_fields: Vec::new(),
            custom: false,
            directory: String::new(),
            isolation: LocalIsolation::default(),
            program: String::new(),
            arguments: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ShellChoice(Option<LocalShell>);
impl std::fmt::Display for ShellChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(shell) => shell.fmt(f),
            None => f.write_str("Custom command"),
        }
    }
}

impl Draft {
    pub(super) fn from_saved(profile: &LocalProfile) -> Self {
        let mut draft = Self {
            directory: profile
                .working_directory
                .as_ref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            isolation: profile.isolation,
            ..Self::default()
        };
        if let Some(command) = &profile.command
            && let Some((program, arguments)) = command.split_first()
        {
            draft.custom = true;
            draft.program.clone_from(program);
            draft.arguments = arguments.to_vec();
        }
        draft.argument_fields = draft
            .arguments
            .iter()
            .map(|_| iced::widget::Id::unique())
            .collect();
        draft
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::SelectShell(shell) => self.custom = shell.is_none(),
            Edit::Directory(directory) if directory.len() <= 4096 => self.directory = directory,
            Edit::Isolation(isolation) => self.isolation = isolation,
            Edit::Program(program) if self.custom && program.len() <= 8192 => {
                self.program = program;
            }
            Edit::AddArgument if self.custom && self.arguments.len() < MAX_ARGUMENTS => {
                self.arguments.push(String::new());
                self.argument_fields.push(iced::widget::Id::unique());
            }
            Edit::Argument(index, value) if self.custom && value.len() <= 8192 => {
                if let Some(argument) = self.arguments.get_mut(index) {
                    *argument = value;
                }
            }
            Edit::RemoveArgument(index) if self.custom && index < self.arguments.len() => {
                self.arguments.remove(index);
                self.argument_fields.remove(index);
            }
            _ => {}
        }
    }

    pub(super) fn edited_focus(&self, edit: &Edit) -> Option<iced::widget::Id> {
        match edit {
            Edit::AddArgument => self.argument_fields.last().cloned(),
            Edit::RemoveArgument(index) => Some(
                self.argument_fields
                    .get(index.saturating_sub(1))
                    .cloned()
                    .unwrap_or_else(|| self.program_field.clone()),
            ),
            Edit::SelectShell(None) => Some(self.program_field.clone()),
            _ => None,
        }
    }

    pub(super) fn command(&self) -> Option<Vec<String>> {
        self.custom.then(|| {
            std::iter::once(self.program.clone())
                .chain(self.arguments.iter().cloned())
                .collect()
        })
    }

    fn shell(&self, preset: LocalShell) -> LocalShell {
        if self.custom {
            LocalShell::Default
        } else {
            preset
        }
    }
}

impl Dialog {
    pub(super) fn local_profile(&self) -> Option<LocalProfile> {
        let saved = self.selected_local();
        if self.selected_local.is_some() && saved.is_none() {
            return None;
        }
        let directory = self.local.directory.trim();
        let mut profile = LocalProfile {
            id: saved.map_or(0, |saved| saved.id),
            isolation: self.local.isolation,
            name: saved.map_or_else(|| "Local Shell".into(), |saved| saved.name.clone()),
            shell: self.local.shell(self.local_shell),
            command: self.local.command(),
            working_directory: (!directory.is_empty()).then(|| PathBuf::from(directory)),
            shell_integration: self.shell_integration,
            emulation: self.local_emulation.term_name().into(),
            terminal_effect: saved.and_then(|saved| saved.terminal_effect),
            terminal: saved.map_or_else(Default::default, |saved| saved.terminal.clone()),
        };
        if saved.is_some_and(|saved| !profile.same_launch(saved)) {
            profile.id = 0;
        }
        Some(profile)
    }
}

impl Workspace {
    pub(super) fn quick_local_view<'a>(&self, dialog: &'a Dialog) -> Element<'a, Message> {
        use crate::workspace::dialog_design::{keyboard_table, secondary_button};
        let draft = &dialog.local;
        let enabled = dialog.protocol == super::Protocol::Local;
        let folder = dialog.selected_local.and_then(|id| {
            let store = dialog.store.as_ref()?;
            store
                .group_of(id)
                .and_then(|path| store.inherited_group_isolation(path))
        });
        let (isolation, source) = draft.isolation.resolve(
            self.default_local_isolation(),
            folder,
            self.clipboard.policy.minimum_isolation,
        );
        let mut shells = available_local_shells();
        if !shells.contains(&dialog.local_shell) {
            shells.push(dialog.local_shell);
        }
        let selected = ShellChoice((!draft.custom).then_some(dialog.local_shell));
        let shell: Element<'a, Message> = if enabled {
            pick_list(
                shells
                    .into_iter()
                    .map(|v| ShellChoice(Some(v)))
                    .chain(std::iter::once(ShellChoice(None)))
                    .collect::<Vec<_>>(),
                Some(selected),
                |v| Message::Local(Edit::SelectShell(v.0)),
            )
            .width(180)
            .text_size(13)
            .keyboard_navigation(draft.shell_picker.clone())
            .on_open(Message::FocusWidget(draft.shell_picker.clone()))
            .into()
        } else {
            button(row![
                text(selected.to_string())
                    .size(13)
                    .width(iced::Length::Fill),
                text("▾").size(13)
            ])
            .width(180)
            .style(secondary_button)
            .into()
        };
        let mut fields = column![
            labeled("Shell", shell),
            labeled(
                "Custom command",
                text_input("Executable path or name", &draft.program)
                    .style(crate::workspace::dialog_design::form_input)
                    .size(13)
                    .id(draft.program_field.clone())
                    .on_input_maybe(
                        (enabled && draft.custom).then_some(|v| Message::Local(Edit::Program(v)))
                    )
            ),
        ]
        .spacing(10);
        if draft.custom {
            for (index, argument) in draft.arguments.iter().enumerate() {
                fields = fields.push(labeled(
                    "Argument",
                    row![
                        text_input("Exact argument", argument)
                            .style(crate::workspace::dialog_design::form_input)
                            .size(13)
                            .id(draft.argument_fields[index].clone())
                            .on_input_maybe(
                                enabled
                                    .then_some(move |v| Message::Local(Edit::Argument(index, v)))
                            ),
                        keyboard_table::action(
                            button(text("Remove").size(13))
                                .on_press_maybe(
                                    enabled.then_some(Message::Local(Edit::RemoveArgument(index)))
                                )
                                .into(),
                            enabled.then_some(Message::Local(Edit::RemoveArgument(index))),
                            true
                        ),
                    ]
                    .spacing(8),
                ));
            }
            let add = (enabled && draft.arguments.len() < MAX_ARGUMENTS)
                .then_some(Message::Local(Edit::AddArgument));
            fields = fields.push(labeled(
                "",
                keyboard_table::action(
                    button(text("Add argument").size(13))
                        .on_press_maybe(add.clone())
                        .into(),
                    add,
                    true,
                ),
            ));
        }
        fields = fields.push(labeled(
            "Start directory",
            text_input("Absolute path (optional)", &draft.directory)
                .style(crate::workspace::dialog_design::form_input)
                .size(13)
                .on_input_maybe(enabled.then_some(|v| Message::Local(Edit::Directory(v)))),
        ));
        if enabled {
            fields = fields
                .push(labeled(
                    "Session isolation",
                    pick_list(LocalIsolation::ALL, Some(draft.isolation), |v| {
                        Message::Local(Edit::Isolation(v))
                    })
                    .text_size(13),
                ))
                .push(text(format!("Effective isolation · {isolation} · {source}")).size(12));
            let command = draft.command();
            let integration = self.quick_shell_integration_for_command(
                dialog.local_emulation,
                draft.shell(dialog.local_shell),
                command.as_deref(),
                Some(isolation),
            );
            fields = fields.push(labeled(
                "Shell integration",
                checkbox(dialog.shell_integration)
                    .label("Add shell integration automatically")
                    .text_size(13)
                    .on_toggle_maybe(
                        (dialog.shell_integration || integration.is_ok())
                            .then_some(Message::ShellIntegration),
                    ),
            ));
            if let Err(reason) = integration {
                fields = fields.push(text(reason).size(12));
            }
            if dialog.selected_local.is_some() {
                fields = fields.push(
                    button(text("Use an unsaved shell").size(13)).on_press(Message::AdHocLocal),
                );
            }
            if let Some(profile) = dialog.local_profile() {
                if let Err(error) = profile.validate() {
                    fields = fields.push(text(error.to_string()).size(12));
                }
            } else {
                fields = fields.push(text("This saved shell no longer exists.").size(12));
            }
        }
        fields.into()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use kortty_terminal::TerminalProfile;

    use super::*;
    use crate::workspace::quick_connect::{Protocol, Store};

    fn private_profile(name: &str) -> bool {
        const PROFILE: &str = "KORTTY_QUICK_LOCAL_TEST_PROFILE";
        if let Some(root) = std::env::var_os(PROFILE) {
            crate::profile::set_runtime_root(PathBuf::from(root));
            return true;
        }
        // Connecting also persists appearance defaults. Keep that real write
        // and the fixture's Sandbox preference out of sibling tests' profile.
        let scratch = crate::snippets::tests::Profile::new();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--include-ignored", "--nocapture"])
            .env(PROFILE, &scratch.0)
            .env("TMPDIR", &scratch.0)
            .env("TMP", &scratch.0)
            .env("TEMP", &scratch.0)
            .output()
            .unwrap();
        assert!(
            child.status.success() && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        false
    }

    #[test]
    fn quick_local_shell_runs_with_edited_arguments_and_directory_without_changing_the_profile() {
        if private_profile(
            "workspace::quick_connect::local::tests::quick_local_shell_runs_with_edited_arguments_and_directory_without_changing_the_profile",
        ) {
            check_local_shell();
        }
    }

    fn check_local_shell() {
        use crate::session_security::IsolationLevel;
        let scratch = crate::snippets::tests::Profile::new();
        let directory = scratch.0.join("quick launch");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("marker file.txt"), "quick-local-ready\n").unwrap();
        #[cfg(windows)]
        let command = vec![
            PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/cmd.exe")
                .to_str()
                .unwrap()
                .into(),
            "/d".into(),
            "/c".into(),
            "type".into(),
            "original.txt".into(),
        ];
        #[cfg(not(windows))]
        let command = vec!["/bin/cat".into(), "original.txt".into()];
        let final_arg = command.len() - 2;
        let path = scratch.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let group = store.create_group(None, "Development").unwrap();
        store
            .set_group_isolation(&group, None, Some(IsolationLevel::Process))
            .unwrap();
        let saved = store
            .save_local_in_group(
                LocalProfile {
                    id: 0,
                    isolation: LocalIsolation::Inherit,
                    name: "Custom Shell".into(),
                    shell: LocalShell::Default,
                    command: Some(command),
                    working_directory: Some(scratch.0.clone()),
                    shell_integration: false,
                    emulation: TerminalProfile::Xterm.term_name().into(),
                    terminal_effect: None,
                    terminal: crate::connection_store::TerminalOverrides {
                        strict_terminal_mode: Some(true),
                        ..Default::default()
                    },
                },
                Some(&group),
            )
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.default_session_isolation = Some(IsolationLevel::Sandbox);
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.update_quick_connect(id, Message::Loaded(Ok(store)));
        let _ = workspace.update_quick_connect(id, Message::Select(saved));
        let dialog = workspace.quick_connect.as_ref().unwrap();
        assert_eq!(dialog.local_profile().unwrap().id, saved);
        assert!(dialog.can_connect());

        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Ssh));
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Local));
        for edit in [
            Edit::Directory("relative".into()),
            Edit::Argument(final_arg, "marker file.txt".into()),
        ] {
            let _ = workspace.update_quick_connect(id, Message::Local(edit));
        }
        assert!(!workspace.quick_connect.as_ref().unwrap().can_connect());
        let _ = workspace.update_quick_connect(
            id,
            Message::Local(Edit::Directory(directory.to_str().unwrap().into())),
        );
        let _ = workspace.update_quick_connect(id, Message::Connect);
        let tab = workspace.windows[&owner].tab().unwrap();
        let pane = &workspace.sessions[&tab.focused_session().unwrap()];
        assert!(pane.strict_escapes);
        assert!(pane.saved_local.is_none());
        let settings = pane.local_security.as_ref().unwrap();
        assert!(settings.saved_profile.is_none());
        assert_eq!(settings.isolation, IsolationLevel::Process);
        assert_eq!(
            settings.working_directory.as_deref(),
            Some(directory.as_path())
        );

        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let frame = pane.session.frame();
            let output: String = frame
                .screen
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            if output.contains("quick-local-ready") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Local command output: {output:?}; status: {:?}; grid: {}x{}; cells: {}; PID: {:?}; directory: {:?}",
                frame.status,
                frame.screen.columns,
                frame.screen.rows,
                frame.screen.cells.len(),
                pane.session.local_shell_pid(),
                settings.working_directory,
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let store = Store::open(path).unwrap();
        let original = store.local(saved).unwrap();
        assert_eq!(
            original.working_directory.as_deref(),
            Some(scratch.0.as_path())
        );
        assert_eq!(
            original.command.as_ref().unwrap().last().unwrap(),
            "original.txt"
        );
        let preferences = crate::workspace::tab_titles::WindowSettings::read(
            &crate::profile::window_settings_path().unwrap(),
        )
        .unwrap();
        assert_eq!(
            preferences.default_session_isolation,
            Some(IsolationLevel::Sandbox)
        );
    }

    #[test]
    #[ignore = "native PTY concurrency investigation; run explicitly on the test desktop"]
    fn quick_local_short_commands_survive_same_process_concurrency() {
        if !private_profile(
            "workspace::quick_connect::local::tests::quick_local_short_commands_survive_same_process_concurrency",
        ) {
            return;
        }
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..32 {
                        check_local_shell();
                    }
                });
            }
        });
    }
}
