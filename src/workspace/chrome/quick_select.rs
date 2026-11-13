use std::sync::{Arc, Weak};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Font, Task, window};

use crate::session::{Frame, GridSize, SessionId};
use crate::terminal_view::quick_select::{Compiled, Labels, Preferences};
use crate::workspace::terminal_link_dialog;

use super::super::{Message as Desktop, PaneSession, Workspace};
use super::{Action, Message};

#[derive(Default)]
pub(super) struct Controller {
    shown: Option<Active>,
    repeat: Option<(window::Id, Key)>,
    compiled: Option<(Preferences, Compiled)>,
}

impl Controller {
    fn settings(&mut self, preferences: &Preferences) -> &Compiled {
        if self
            .compiled
            .as_ref()
            .is_some_and(|(previous, _)| previous != preferences)
        {
            self.compiled = None;
        }
        &self
            .compiled
            .get_or_insert_with(|| (preferences.clone(), preferences.compile()))
            .1
    }
}

struct Active {
    owner: window::Id,
    session: SessionId,
    grid: GridSize,
    font: (u8, Font),
    viewport: (usize, bool, u16, u16),
    frame: Weak<Frame>,
    labels: Labels,
}

fn viewport(frame: &Frame) -> (usize, bool, u16, u16) {
    (
        frame
            .screen
            .scrollback_lines
            .saturating_sub(frame.scroll_offset),
        frame.screen.alternate_screen,
        frame.screen.rows,
        frame.screen.columns,
    )
}

impl Workspace {
    fn quick_select_pane(
        &self,
        owner: window::Id,
    ) -> Option<(SessionId, &PaneSession, (u8, Font))> {
        if self.exiting
            || self.scheduler_draining
            || !self.palette_can_open(owner)
            || self.palette.is_open(owner)
        {
            return None;
        }
        let window = self.windows.get(&owner)?;
        let session = window.tab()?.focused_session()?;
        let pane = self.sessions.get(&session)?;
        if !window.focused
            || !self.window_contains_visible_pane(owner, session)
            || pane.search.is_some()
            || pane.frame.ibm.is_some()
            || pane.frame.password_prompt
        {
            return None;
        }
        let effect = pane
            .effect
            .is_some_and(|active| self.allowed_terminal_effect(active.selection.effect));
        Some((session, pane, self.pane_terminal_font(pane, effect)))
    }

    pub(in crate::workspace) fn quick_select_available(&self, owner: window::Id) -> bool {
        self.quick_select_pane(owner).is_some()
    }

    pub(in crate::workspace) fn start_quick_select(&mut self, owner: window::Id) -> Task<Desktop> {
        self.refresh_quick_select();
        if self
            .chrome
            .quick_select
            .shown
            .as_ref()
            .is_some_and(|active| active.owner == owner)
        {
            return Task::none();
        }
        let Some((session, pane, font)) = self.quick_select_pane(owner) else {
            return Task::none();
        };
        let grid = pane.grid;
        let windows_paths = cfg!(windows) && pane.session.local_shell_pid().is_some();
        let frame = Arc::clone(&pane.frame);
        let settings = self
            .chrome
            .quick_select
            .settings(&self.window_settings.quick_select);
        let Some(labels) = Labels::capture(&frame, settings, windows_paths) else {
            self.notice(
                owner,
                "No Quick Select matches in the visible terminal.".into(),
            );
            return Task::none();
        };
        self.chrome.quick_select.shown = Some(Active {
            owner,
            session,
            grid,
            font,
            viewport: viewport(&frame),
            frame: Arc::downgrade(&frame),
            labels,
        });
        Task::none()
    }

    pub(in crate::workspace) fn quick_select_labels(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Option<&Labels> {
        self.chrome
            .quick_select
            .shown
            .as_ref()
            .filter(|active| active.owner == owner && active.session == session)
            .map(|active| &active.labels)
    }

    pub(in crate::workspace) fn refresh_quick_select(&mut self) {
        let Some(mut active) = self.chrome.quick_select.shown.take() else {
            return;
        };
        let Some((session, pane, font)) = self.quick_select_pane(active.owner) else {
            return;
        };
        if session != active.session
            || pane.grid != active.grid
            || font != active.font
            || viewport(&pane.frame) != active.viewport
        {
            return;
        }
        if active.frame.as_ptr() != Arc::as_ptr(&pane.frame) {
            if !active.labels.retain_visible(&pane.frame) {
                return;
            }
            active.frame = Arc::downgrade(&pane.frame);
        }
        self.chrome.quick_select.shown = Some(active);
    }

    pub(in crate::workspace) fn interrupt_quick_select(&mut self, message: &Desktop) {
        if let Desktop::Key(owner, key, ..) = message
            && self.suppressed_quick_select_repeat(*owner, key)
        {
            self.chrome.quick_select.repeat = None;
        }
        if let Desktop::Window(owner, window::Event::Unfocused | window::Event::Closed) = message
            && self
                .chrome
                .quick_select
                .repeat
                .as_ref()
                .is_some_and(|(id, _)| id == owner)
        {
            self.chrome.quick_select.repeat = None;
        }
        let Some(active) = self.chrome.quick_select.shown.as_ref() else {
            return;
        };
        let interrupt = match message {
            Desktop::Window(
                owner,
                window::Event::Unfocused
                | window::Event::Closed
                | window::Event::Resized(_)
                | window::Event::Moved(_)
                | window::Event::Rescaled(_),
            )
            | Desktop::FocusPane(owner, _)
            | Desktop::ResizeSplit(owner, _)
            | Desktop::DragPane(owner, _)
            | Desktop::TerminalContext(owner, _)
            | Desktop::Chrome(
                Message::OpenMenu(owner, _) | Message::OpenToolbarOverflow(owner, ..),
            ) => *owner == active.owner,
            Desktop::Scroll(session, _)
            | Desktop::ScrollTo(session, _)
            | Desktop::Selection(session, _)
            | Desktop::TerminalMouse(session, _) => *session == active.session,
            _ => false,
        };
        if interrupt {
            self.chrome.quick_select.shown = None;
        }
    }

    pub(in crate::workspace) fn suppressed_quick_select_repeat(
        &self,
        owner: window::Id,
        key: &Key,
    ) -> bool {
        self.chrome
            .quick_select
            .repeat
            .as_ref()
            .is_some_and(|(id, suppressed)| *id == owner && suppressed == key)
    }

    pub(in crate::workspace) fn quick_select_key(
        &mut self,
        owner: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<Desktop>> {
        let controller = &mut self.chrome.quick_select;
        if controller.shown.as_ref()?.owner != owner {
            return None;
        }
        if matches!(
            key,
            Key::Named(
                Named::Alt
                    | Named::AltGraph
                    | Named::CapsLock
                    | Named::Control
                    | Named::Fn
                    | Named::Meta
                    | Named::NumLock
                    | Named::ScrollLock
                    | Named::Shift
                    | Named::Super
            ) | Key::Unidentified
        ) || self.window_settings.key_bindings.action_for(key, modifiers)
            == Some(Action::QuickSelect)
        {
            return Some(Task::none());
        }
        let mut active = controller.shown.take()?;
        controller.repeat = Some((owner, key.clone()));
        if modifiers.control() || modifiers.alt() || modifiers.logo() || modifiers.command() {
            return Some(Task::none());
        }
        if matches!(key, Key::Named(Named::Backspace)) {
            active.labels.backspace();
            self.chrome.quick_select.shown = Some(active);
            return Some(Task::none());
        }
        let Key::Character(value) = key else {
            return Some(Task::none());
        };
        let [letter] = value.as_bytes() else {
            return Some(Task::none());
        };
        if !letter.is_ascii_alphabetic() {
            return Some(Task::none());
        }
        let Some((session, pane, font)) = self.quick_select_pane(owner) else {
            return Some(Task::none());
        };
        let latest = pane.session.frame();
        if active.session != session
            || active.grid != pane.grid
            || active.font != font
            || latest.password_prompt
            || active.viewport != viewport(&latest)
            || !active.labels.retain_visible(&latest)
        {
            return Some(Task::none());
        }
        let Some(target) = active
            .labels
            .letter(char::from(letter.to_ascii_lowercase()))
        else {
            self.chrome.quick_select.shown = Some(active);
            return Some(Task::none());
        };
        if modifiers.shift()
            && let Some(link) = target.link()
        {
            return Some(self.update_terminal_link(
                owner,
                terminal_link_dialog::Message::Open(active.session, Arc::new(link)),
            ));
        }
        if modifiers.shift()
            && let Some(file) = target.file()
            && let Some(task) = self.open_terminal_file(owner, active.session, file)
        {
            return Some(task);
        }
        self.notice(owner, "Copied Quick Select match.".into());
        Some(self.write_clipboard(target.text.clone()))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use iced::keyboard::Location;
    use kortty_terminal::TerminalProfile;

    use crate::session::Session;

    use super::*;

    #[test]
    fn quick_select_edit_menu_copies_and_suppresses_terminal_repeat() {
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        workspace.window_settings.quick_select = Preferences {
            alphabet: "zx".into(),
            patterns: vec!["TICKET-[0-9]+".into()],
        };
        let (owner, _) = workspace.open_window(false);
        workspace.windows.get_mut(&owner).unwrap().focused = true;
        assert!(!workspace.quick_select_available(owner));
        let grid = GridSize::default();
        let shell = Session::start(grid, TerminalProfile::Xterm).unwrap();
        let id = workspace.store_session(shell, grid, 14);
        workspace.add_tab(owner, id);
        let shell = &workspace.sessions[&id].session;
        shell
            .send(b"printf '\\033[2J\\033[HTICKET-64208\\n'; read line\r".to_vec())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shell.frame().screen.row(0).is_some_and(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .starts_with("TICKET-64208")
        }) {
            assert!(
                Instant::now() < deadline,
                "terminal output did not reach the viewport"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = workspace.update(Desktop::SessionUpdate(id));
        let _ = workspace.update(Desktop::Chrome(Message::Invoke(owner, Action::QuickSelect)));
        assert!(workspace.quick_select_labels(owner, id).is_some());
        let key = Key::Character("z".into());
        let _ = workspace.update(Desktop::Key(
            owner,
            key.clone(),
            Location::Standard,
            Modifiers::empty(),
            Some("z".into()),
        ));
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("TICKET-64208")
        );
        assert!(workspace.quick_select_labels(owner, id).is_none());
        assert!(workspace.suppressed_quick_select_repeat(owner, &key));
        let repeat = workspace.update(Desktop::RepeatedKey(
            owner,
            key,
            Location::Standard,
            Modifiers::empty(),
            Some("z".into()),
        ));
        assert_eq!(repeat.units(), 0);
        let _ = workspace.sessions[&id].session.stop().recv_blocking();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use iced::futures::{StreamExt, executor::block_on};
    use iced::keyboard::Location;
    use kortty_pty::{LaunchOptions, LocalShell};
    use kortty_terminal::TerminalProfile;

    use crate::session::Session;

    use super::*;

    #[test]
    fn quick_select_windows_shift_opens_a_conpty_relative_file() {
        const DISPLAYED_FILE: &str = r".\kortty-path-fixture.txt:2:3";
        let directory = std::env::temp_dir().join(crate::private_files::nonce().unwrap());
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("kortty-path-fixture.txt");
        let mut workspace = Workspace::default();
        let mut session_id = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::fs::write(&file, "first\nsecond\n").unwrap();
            workspace.clipboard.policy.internal = true;
            workspace.window_settings.quick_select = Preferences {
                alphabet: "zx".into(),
                patterns: Vec::new(),
            };
            let (owner, _) = workspace.open_window(false);
            workspace.windows.get_mut(&owner).unwrap().focused = true;
            let grid = GridSize::default();
            let session = Session::start_local(
                grid,
                TerminalProfile::Xterm,
                LaunchOptions {
                    shell: LocalShell::CommandPrompt,
                    working_directory: Some(directory.clone()),
                    shell_integration: true,
                    ..LaunchOptions::default()
                },
                true,
                None,
            )
            .unwrap();
            let id = workspace.store_session(session, grid, 14);
            session_id = Some(id);
            workspace.add_tab(owner, id);
            let shell = &workspace.sessions[&id].session;
            let deadline = Instant::now() + Duration::from_secs(10);
            while shell.local_shell_pid().is_none()
                || shell.frame().shell_activity().code_prompt.state
                    != crate::session::PromptState::Ready
            {
                assert!(Instant::now() < deadline, "CMD fixture did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!("owned Quick Select CMD PID {:?}", shell.local_shell_pid());
            shell
                .send(b"cls & echo .\\kortty-path-fixture.txt:2:3\r".to_vec())
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let frame = shell.frame();
                if frame.shell_activity().code_prompt.state == crate::session::PromptState::Ready
                    && (0..frame.screen.rows).any(|row| {
                        frame.screen.row(row).is_some_and(|cells| {
                            cells
                                .iter()
                                .map(|cell| cell.text.as_str())
                                .collect::<String>()
                                .starts_with(DISPLAYED_FILE)
                        })
                    })
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "CMD fixture did not reach its prompt"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = workspace.update(Desktop::SessionUpdate(id));
            let select_file = |workspace: &mut Workspace, modifiers: Modifiers| {
                let _ =
                    workspace.update(Desktop::Chrome(Message::Invoke(owner, Action::QuickSelect)));
                let label = workspace
                    .quick_select_labels(owner, id)
                    .and_then(|labels| labels.label_for_text(DISPLAYED_FILE))
                    .expect("Quick Select labeled the relative file")
                    .to_owned();
                let mut task = Task::none();
                for mut letter in label.chars() {
                    if modifiers.shift() {
                        letter = letter.to_ascii_uppercase();
                    }
                    let text = letter.to_string();
                    task = workspace.update(Desktop::Key(
                        owner,
                        Key::Character(text.clone().into()),
                        Location::Standard,
                        modifiers,
                        Some(text),
                    ));
                }
                task
            };
            let _ = select_file(&mut workspace, Modifiers::empty());
            assert_eq!(
                workspace.clipboard.buffer.get().as_deref(),
                Some(DISPLAYED_FILE)
            );
            assert!(workspace.editors.is_empty());
            workspace.clipboard.buffer.set("clipboard sentinel".into());
            let task = select_file(&mut workspace, Modifiers::SHIFT);
            let mut actions = iced_runtime::task::into_stream(task).expect("file open task");
            let action = block_on(actions.next()).expect("file open result");
            let iced_runtime::Action::Output(Desktop::TerminalFileLoaded(ticket, result)) = action
            else {
                panic!("expected file open result");
            };
            let _ = workspace.finish_terminal_file(ticket, Ok(result.unwrap()));
            let opened = workspace
                .editors
                .values()
                .find_map(crate::editor_window::EditorWindow::local_document_path)
                .expect("the editor opened the selected file");
            assert_eq!(
                opened.file_name(),
                Some(Path::new("kortty-path-fixture.txt").as_os_str())
            );
            assert_eq!(
                workspace.clipboard.buffer.get().as_deref(),
                Some("clipboard sentinel")
            );
        }));
        if let Some(id) = session_id {
            let _ = workspace.sessions[&id].session.stop().recv_blocking();
        }
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&directory);
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}
