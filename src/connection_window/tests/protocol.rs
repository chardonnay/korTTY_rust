use super::*;
use crate::connection_store::protocol::{Kind, Profile as SavedProfile};
use crate::connection_window::form::{Edit, LoginEdit};
use crate::connection_window::local::Edit as Local;

#[test]
fn new_connection_can_switch_before_entering_a_host_and_cancel_without_writing() {
    let directory = Profile::new();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::New);
    let _ = manager.update(Message::Edit(Edit::Name("Local draft".into())));
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert!(matches!(
        manager.draft.as_ref().unwrap().prepared().unwrap().selected,
        SavedProfile::Local(_)
    ));
    let _ = manager.update(Message::Local(Local::Custom(true)));
    let _ = manager.update(Message::Local(Local::Program("echo".into())));
    let _ = manager.update(Message::Protocol(Kind::Ssh));
    assert!(manager.draft.as_ref().unwrap().prepared().is_err());
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert_eq!(edited_local(&manager).command, Some(vec!["echo".into()]));
    let _ = manager.update(Message::CancelEdit);
    assert!(
        Store::open(directory.path())
            .unwrap()
            .local_profiles()
            .is_empty()
    );
}

#[test]
fn one_editor_saves_all_protocols_with_stable_identity_and_retained_settings() {
    let directory = Profile::new();
    let mut store = Store::open(directory.path()).unwrap();
    let mut original = connection("Protocol fixture", "retained tag");
    original.terminal.encoding = Some(kortty_terminal::TextEncoding::Latin1);
    original.window_geometry = Some(crate::connection_store::WindowGeometry {
        width: 900,
        ..Default::default()
    });
    let id = store.save(original).unwrap();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::Select(id));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Local));
    let _ = manager.update(Message::Local(Local::Custom(true)));
    let _ = manager.update(Message::Local(Local::Program("echo".into())));
    let _ = manager.update(Message::Local(Local::AddArgument));
    let _ = manager.update(Message::Local(Local::Argument(0, " exact value ".into())));
    complete_persisted(&mut manager, Message::Save);
    let local = manager.store.local(id).unwrap();
    assert_eq!(
        local.terminal.encoding,
        Some(kortty_terminal::TextEncoding::Latin1)
    );
    assert!(manager.store.get(id).is_none());
    assert_eq!(manager.store.retained_protocols(id).len(), 1);

    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::SelectLocal(id));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Serial));
    let _ = manager.update(Message::Serial(crate::serial_dialog::Message::Device(
        "COM24".into(),
    )));
    let _ = manager.update(Message::Serial(crate::serial_dialog::Message::Baud(
        "19200".into(),
    )));
    complete_persisted(&mut manager, Message::Save);
    assert_eq!(
        manager
            .store
            .serial(id)
            .unwrap()
            .connection
            .settings
            .baud_rate,
        19200
    );

    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::SelectSerial(id));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Ssh));
    let _ = manager.update(Message::Edit(Edit::Name("Renamed protocol fixture".into())));
    complete_persisted(&mut manager, Message::Save);
    let ssh = manager.store.get(id).unwrap();
    assert_eq!(ssh.name, "Renamed protocol fixture");
    assert_eq!(ssh.tag, "retained tag");
    assert_eq!(ssh.window_geometry.as_ref().unwrap().width, 900);
    assert_eq!(
        ssh.terminal.encoding,
        Some(kortty_terminal::TextEncoding::Latin1)
    );
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert_eq!(
        edited_local(&manager).command,
        Some(vec!["echo".into(), " exact value ".into()])
    );
    let _ = manager.update(Message::CancelEdit);
}

#[test]
fn protocol_editor_rejects_a_changed_source_revision() {
    let directory = Profile::new();
    let mut store = Store::open(directory.path()).unwrap();
    let id = store.save(connection("First", "")).unwrap();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::Select(id));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Local));
    let mut changed = store.get(id).unwrap().clone();
    changed.name = "External".into();
    store.save(changed).unwrap();
    manager.update_store(store.clone());
    assert!(manager.draft_conflict);
    assert!(iced_runtime::task::into_stream(manager.update(Message::Save)).is_none());
    assert_eq!(
        Store::open(directory.path()).unwrap().get(id).unwrap().name,
        "External"
    );
}

#[test]
fn incomplete_inactive_ssh_edits_are_not_silently_discarded() {
    let directory = Profile::new();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::New);
    let _ = manager.update(Message::Edit(Edit::Name("Draft".into())));
    let _ = manager.update(Message::Edit(Edit::Login(
        false,
        LoginEdit::Host("bad host".into()),
    )));
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert!(
        manager
            .draft
            .as_ref()
            .unwrap()
            .prepared()
            .unwrap_err()
            .contains("SSH")
    );
    let _ = manager.update(Message::Protocol(Kind::Ssh));
    let _ = manager.update(Message::Edit(Edit::Login(
        false,
        LoginEdit::Host("fixed.example".into()),
    )));
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert!(manager.draft.as_ref().unwrap().prepared().is_ok());
}

#[test]
fn switching_to_local_preserves_an_explicit_isolation_level() {
    let directory = Profile::new();
    let mut store = Store::open(directory.path()).unwrap();
    let mut ssh = connection("Isolated", "");
    ssh.isolation = IsolationLevel::Sandbox;
    ssh.inherit_isolation = false;
    let id = store.save(ssh).unwrap();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::Select(id));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert_eq!(
        edited_local(&manager).isolation,
        crate::session_security::LocalIsolation::Sandbox
    );
}

#[test]
fn shell_selector_retains_custom_arguments_and_inactive_callbacks_are_ignored() {
    let directory = Profile::new();
    let mut store = Store::open(directory.path()).unwrap();
    let id = store.save(connection("Protocol controls", "")).unwrap();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::Select(id));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Protocol(Kind::Local));
    let _ = manager.update(Message::Local(Local::SelectShell(None)));
    let _ = manager.update(Message::Local(Local::Program("echo".into())));
    let _ = manager.update(Message::Local(Local::AddArgument));
    let _ = manager.update(Message::Local(Local::Argument(
        0,
        " retained argument ".into(),
    )));
    let _ = manager.update(Message::Local(Local::SelectShell(Some(
        kortty_pty::LocalShell::Bash,
    ))));
    assert_eq!(edited_local(&manager).shell, kortty_pty::LocalShell::Bash);
    assert!(edited_local(&manager).command.is_none());
    let _ = manager.update(Message::Local(Local::SelectShell(None)));
    let expected = vec!["echo".to_owned(), " retained argument ".to_owned()];
    assert_eq!(edited_local(&manager).command, Some(expected.clone()));
    let _ = manager.update(Message::Protocol(Kind::Ssh));
    let _ = manager.update(Message::Local(Local::Program("discarded callback".into())));
    let _ = manager.update(Message::Serial(crate::serial_dialog::Message::Baud(
        "38400".into(),
    )));
    let _ = manager.update(Message::Protocol(Kind::Local));
    assert_eq!(edited_local(&manager).command, Some(expected));
    let _ = manager.update(Message::Protocol(Kind::Serial));
    let _ = manager.update(Message::Serial(crate::serial_dialog::Message::Device(
        "COM24".into(),
    )));
    assert_eq!(edited_serial(&manager).connection.settings.baud_rate, 9600);
}

#[test]
fn opening_protocol_selectors_does_not_mark_a_new_draft_changed() {
    let directory = Profile::new();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::NewLocal);
    let _ = manager.update(Message::Local(Local::Focus(iced::widget::Id::unique())));
    assert!(!manager.draft.as_ref().unwrap().changed);
    let _ = manager.update(Message::CancelEdit);
    let _ = manager.update(Message::NewSerial);
    let _ = manager.update(Message::Serial(crate::serial_dialog::Message::Focus(
        iced::widget::Id::unique(),
    )));
    assert!(!manager.draft.as_ref().unwrap().changed);
    let _ = manager.update(Message::CancelEdit);
    assert!(manager.store.local_profiles().is_empty());
    assert!(manager.store.serial_profiles().is_empty());
}
