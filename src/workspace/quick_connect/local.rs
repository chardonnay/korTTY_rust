use std::path::PathBuf;

use iced::Element;
use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use kortty_pty::{LocalShell, available_local_shells};

use crate::connection_store::LocalProfile;

use super::{Dialog, Message, Workspace, view::labeled};

const MAX_ARGUMENTS: usize = 127;

#[derive(Clone, Debug)]
pub enum Edit {
    Custom(bool),
    Directory(String),
    Program(String),
    AddArgument,
    Argument(usize, String),
    RemoveArgument(usize),
}

#[derive(Default)]
pub(super) struct Draft {
    custom: bool,
    directory: String,
    program: String,
    arguments: Vec<String>,
}

impl Draft {
    pub(super) fn from_saved(profile: &LocalProfile) -> Self {
        let mut draft = Self {
            directory: profile
                .working_directory
                .as_ref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            ..Self::default()
        };
        if let Some(command) = &profile.command
            && let Some((program, arguments)) = command.split_first()
        {
            draft.custom = true;
            draft.program.clone_from(program);
            draft.arguments = arguments.to_vec();
        }
        draft
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Custom(custom) => self.custom = custom,
            Edit::Directory(directory) if directory.len() <= 4096 => self.directory = directory,
            Edit::Program(program) if self.custom && program.len() <= 8192 => {
                self.program = program;
            }
            Edit::AddArgument if self.custom && self.arguments.len() < MAX_ARGUMENTS => {
                self.arguments.push(String::new());
            }
            Edit::Argument(index, value) if self.custom && value.len() <= 8192 => {
                if let Some(argument) = self.arguments.get_mut(index) {
                    *argument = value;
                }
            }
            Edit::RemoveArgument(index) if self.custom && index < self.arguments.len() => {
                self.arguments.remove(index);
            }
            _ => {}
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
            name: saved.map_or_else(|| "Local Shell".into(), |saved| saved.name.clone()),
            shell: self.local.shell(self.local_shell),
            command: self.local.command(),
            working_directory: (!directory.is_empty()).then(|| PathBuf::from(directory)),
            shell_integration: self.shell_integration,
            emulation: self.ssh.profile().term_name().into(),
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
        let draft = &dialog.local;
        let mut fields = column![
            labeled(
                "",
                checkbox(draft.custom)
                    .label("Run a custom program")
                    .on_toggle(|custom| Message::Local(Edit::Custom(custom))),
            ),
            labeled(
                "Starting directory",
                text_input("Absolute path (optional)", &draft.directory)
                    .on_input(|path| Message::Local(Edit::Directory(path))),
            ),
        ]
        .spacing(8);
        if draft.custom {
            fields = fields.push(labeled(
                "Executable",
                text_input("Executable path or name", &draft.program)
                    .on_input(|program| Message::Local(Edit::Program(program))),
            ));
            for (index, argument) in draft.arguments.iter().enumerate() {
                fields = fields.push(labeled(
                    "",
                    row![
                        text_input(&format!("Argument {}", index + 1), argument)
                            .on_input(move |value| Message::Local(Edit::Argument(index, value))),
                        button("Remove").on_press(Message::Local(Edit::RemoveArgument(index))),
                    ]
                    .spacing(8),
                ));
            }
            fields = fields.push(labeled(
                "",
                button("Add argument").on_press_maybe(
                    (draft.arguments.len() < MAX_ARGUMENTS)
                        .then_some(Message::Local(Edit::AddArgument)),
                ),
            ));
        } else {
            let mut shells = available_local_shells();
            if !shells.contains(&dialog.local_shell) {
                shells.push(dialog.local_shell);
            }
            fields = fields.push(labeled(
                "Shell",
                pick_list(shells, Some(dialog.local_shell), Message::LocalShell),
            ));
        }
        let command = draft.command();
        let integration = self.quick_shell_integration_for_command(
            dialog.local_emulation,
            draft.shell(dialog.local_shell),
            command.as_deref(),
        );
        fields = fields.push(labeled(
            "Shell integration",
            checkbox(dialog.shell_integration)
                .label("Add shell integration automatically")
                .on_toggle_maybe(
                    (dialog.shell_integration || integration.is_ok())
                        .then_some(Message::ShellIntegration),
                ),
        ));
        if let Err(reason) = integration {
            fields = fields.push(text(reason).size(13));
        }
        if dialog.selected_local.is_some() {
            fields = fields.push(button("Use an unsaved shell").on_press(Message::AdHocLocal));
        }
        if let Some(profile) = dialog.local_profile() {
            if let Err(error) = profile.validate() {
                fields = fields.push(text(error.to_string()).size(13));
            }
        } else {
            fields = fields.push(text("This saved shell no longer exists.").size(13));
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

    #[test]
    fn quick_local_shell_runs_with_edited_arguments_and_directory_without_changing_the_profile() {
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
        let saved = store
            .save_local(LocalProfile {
                id: 0,
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
            })
            .unwrap();
        let mut workspace = Workspace::default();
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
                "Local command output: {output:?}"
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
    }
}
