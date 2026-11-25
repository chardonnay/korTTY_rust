use super::{Dialog, Protocol};
use crate::connection_store::protocol::EditorSettings;
use crate::terminal_logging::Form;

impl Dialog {
    pub(super) fn logging_form(&self) -> &Form {
        match self.protocol {
            Protocol::Local => &self.local_logging,
            Protocol::Serial => &self.serial_logging,
            _ => self.ssh.logging_form(),
        }
    }

    pub(super) fn logging_form_mut(&mut self) -> &mut Form {
        match self.protocol {
            Protocol::Local => &mut self.local_logging,
            Protocol::Serial => &mut self.serial_logging,
            _ => self.ssh.logging_form_mut(),
        }
    }

    pub(super) fn cancel_logging_pickers(&mut self) {
        self.ssh.logging_form_mut().cancel_folder_picker();
        self.local_logging.cancel_folder_picker();
        self.serial_logging.cancel_folder_picker();
    }

    pub(super) fn selected_incognito(&self) -> bool {
        match self.protocol {
            Protocol::Ssh | Protocol::Sftp => self.selected_ssh_incognito(),
            _ => self
                .selected_profile_id()
                .and_then(|id| self.store.as_ref()?.editor_settings(id))
                .is_some_and(|settings| settings.incognito),
        }
    }

    pub(super) fn native_editor_settings(&self) -> std::io::Result<EditorSettings> {
        let mut settings = self
            .selected_profile_id()
            .and_then(|id| self.store.as_ref()?.editor_settings(id))
            .cloned()
            .unwrap_or_default();
        settings.logging = self.logging_form().config()?;
        Ok(settings)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::{Message, Protocol, Workspace};
    use crate::session::{SessionId, Status};
    use crate::terminal_logging::{Config, Edit};
    use std::{
        io::Write,
        time::{Duration, Instant},
    };

    fn wait(mut ready: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(
                Instant::now() < until,
                "quick-connect worker did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn open(workspace: &mut Workspace, protocol: Protocol) -> (iced::window::Id, iced::window::Id) {
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        let _ = workspace.update_quick_connect(id, Message::Protocol(protocol));
        (owner, id)
    }

    fn enable(workspace: &mut Workspace, id: iced::window::Id, folder: &std::path::Path) {
        std::fs::create_dir(folder).unwrap();
        for edit in [
            Edit::Enabled(true),
            Edit::Folder(folder.display().to_string()),
            Edit::Compress(false),
        ] {
            let _ = workspace
                .update_quick_connect(id, Message::Ssh(crate::ssh_dialog::Message::Logging(edit)));
        }
    }

    fn stop(workspace: &mut Workspace, id: SessionId) {
        let _ = workspace.sessions[&id].session.stop().recv_blocking();
        assert!(workspace.sessions[&id].session.logfile_notice().is_none());
        workspace.stop_session(id);
        for done in std::mem::take(&mut workspace.pending_shutdown) {
            let _ = done.recv_blocking();
        }
    }

    fn output(workspace: &Workspace, id: SessionId, text: &str) -> bool {
        workspace.sessions[&id]
            .session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains(text)
    }

    fn log(folder: &std::path::Path) -> String {
        let logs: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .map(|f| f.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "log"))
            .collect();
        assert_eq!(logs.len(), 1);
        std::fs::read_to_string(&logs[0]).unwrap()
    }

    #[test]
    fn quick_local_logging_captures_first_output_and_preserves_selected_emulation() {
        use super::super::local::Edit as Local;
        let scratch = crate::snippets::tests::Profile::new();
        let folder = scratch.0.join("logs");
        let mut workspace = Workspace::default();
        let (owner, id) = open(&mut workspace, Protocol::Local);
        enable(&mut workspace, id, &folder);
        for message in [
            Message::Profile(kortty_terminal::TerminalProfile::Vt220),
            Message::Local(Local::SelectShell(None)),
            Message::Local(Local::Program("/bin/sh".into())),
            Message::Local(Local::AddArgument),
            Message::Local(Local::Argument(0, "-c".into())),
            Message::Local(Local::AddArgument),
            Message::Local(Local::Argument(
                1,
                "printf 'QUICK LOCAL FIRST\\n'; exec /bin/cat".into(),
            )),
            Message::Connect,
        ] {
            let _ = workspace.update_quick_connect(id, message);
        }
        assert!(workspace.quick_connect.is_none());
        let session = workspace.windows[&owner]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        assert_eq!(
            workspace.sessions[&session].session.frame().screen.profile,
            kortty_terminal::TerminalProfile::Vt220
        );
        wait(|| output(&workspace, session, "QUICK LOCAL FIRST"));
        stop(&mut workspace, session);
        assert!(log(&folder).contains("QUICK LOCAL FIRST"));
    }

    #[test]
    fn quick_serial_logging_captures_output_without_a_saved_profile() {
        let scratch = crate::snippets::tests::Profile::new();
        let folder = scratch.0.join("logs");
        let (mut peer, device) = crate::session::serial_test_pair().unwrap();
        let mut workspace = Workspace::default();
        let (owner, id) = open(&mut workspace, Protocol::Serial);
        enable(&mut workspace, id, &folder);
        let _ = workspace.update_quick_connect(
            id,
            Message::Serial(crate::serial_dialog::Message::Device(
                device.display().to_string(),
            )),
        );
        let _ = workspace.update_quick_connect(id, Message::Connect);
        assert!(workspace.quick_connect.is_none());
        let session = workspace.windows[&owner]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        wait(|| workspace.sessions[&session].session.frame().status == Status::Running);
        peer.write_all(b"QUICK SERIAL FIRST\r\n").unwrap();
        wait(|| output(&workspace, session, "QUICK SERIAL FIRST"));
        stop(&mut workspace, session);
        assert!(log(&folder).contains("QUICK SERIAL FIRST"));
    }

    #[test]
    fn quick_logging_drafts_and_saved_common_settings_are_protocol_specific() {
        let scratch = crate::snippets::tests::Profile::new();
        let mut workspace = Workspace::default();
        let (_, id) = open(&mut workspace, Protocol::Ssh);
        enable(&mut workspace, id, &scratch.0.join("ssh"));
        let ssh = workspace
            .quick_connect
            .as_ref()
            .unwrap()
            .logging_form()
            .config()
            .unwrap();
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Local));
        assert!(
            !workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .logging_form()
                .config()
                .unwrap()
                .enabled()
        );
        enable(&mut workspace, id, &scratch.0.join("local"));
        let local = workspace
            .quick_connect
            .as_ref()
            .unwrap()
            .native_editor_settings()
            .unwrap()
            .logging;
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Serial));
        assert_eq!(
            workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .logging_form()
                .config()
                .unwrap(),
            Config::default()
        );
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Ssh));
        assert_eq!(
            workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .logging_form()
                .config()
                .unwrap(),
            ssh
        );
        let _ = workspace.update_quick_connect(id, Message::Protocol(Protocol::Local));
        assert_eq!(
            workspace
                .quick_connect
                .as_ref()
                .unwrap()
                .logging_form()
                .config()
                .unwrap(),
            local
        );
    }

    #[test]
    fn private_native_profiles_refuse_logging_edits_and_save_before_launch() {
        use crate::connection_store::{SerialProfile, Store, protocol::Profile};
        use crate::session::SerialConnection;
        use kortty_terminal::TerminalProfile;

        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("connections.json");
        let mut workspace = Workspace::default();
        let (_, id) = open(&mut workspace, Protocol::Local);
        let local = workspace
            .quick_connect
            .as_ref()
            .unwrap()
            .local_profile()
            .unwrap();
        let serial = SerialProfile::new(
            "Private serial",
            SerialConnection {
                device: "/dev/private-test-port".into(),
                ..Default::default()
            },
            TerminalProfile::Xterm,
        );
        let mut form = crate::terminal_logging::Form::new(Config::default());
        form.update(Edit::Enabled(true));
        let logging = form.config().unwrap();
        let settings = super::EditorSettings {
            incognito: true,
            logging: logging.clone(),
            ..Default::default()
        };
        let mut store = Store::open(path.clone()).unwrap();
        for profile in [
            Profile::Local(Box::new(local)),
            Profile::Serial(Box::new(serial)),
        ] {
            store
                .save_protocol(None, profile, Vec::new(), None, Some(settings.clone()))
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let _ = workspace.update_quick_connect(id, Message::Loaded(Ok(store)));
        for (saved, protocol, status) in [
            (
                1,
                Protocol::Local,
                "A private terminal cannot save a shell profile.",
            ),
            (
                2,
                Protocol::Serial,
                "A private terminal cannot save a serial profile.",
            ),
        ] {
            let _ = workspace.update_quick_connect(id, Message::Select(saved));
            let dialog = workspace.quick_connect.as_ref().unwrap();
            assert_eq!(dialog.protocol, protocol);
            assert!(dialog.selected_incognito());
            assert!(!workspace.quick_logging_available(dialog));
            let _ = workspace.update_quick_connect(
                id,
                Message::Ssh(crate::ssh_dialog::Message::Logging(Edit::Enabled(false))),
            );
            assert_eq!(
                workspace
                    .quick_connect
                    .as_ref()
                    .unwrap()
                    .logging_form()
                    .config()
                    .unwrap(),
                logging
            );
            let _ = workspace.update_quick_connect(id, Message::SaveConnection(true));
            let _ = workspace.update_quick_connect(id, Message::Connect);
            assert_eq!(
                workspace.quick_connect.as_ref().unwrap().status.as_deref(),
                Some(status)
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(workspace.sessions.is_empty());
        }
    }
}
