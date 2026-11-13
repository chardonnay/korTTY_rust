use crate::sftp_window::{Message as WindowMessage, Side};

use super::*;

const NAME: u8 = 104;

pub(super) fn realpath(bytes: &mut Vec<u8>, id: u32) {
    let mut body = 1u32.to_be_bytes().to_vec();
    body.extend_from_slice(&7u32.to_be_bytes());
    body.extend_from_slice(b"/remote");
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    packet(bytes, NAME, id, &body);
}

fn stage_attributes(bytes: &mut Vec<u8>, id: u32, length: u64) {
    let mut body = 15u32.to_be_bytes().to_vec();
    body.extend_from_slice(&length.to_be_bytes());
    body.extend_from_slice(&1000u32.to_be_bytes());
    body.extend_from_slice(&1001u32.to_be_bytes());
    body.extend_from_slice(&0o100600u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&24u32.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn verify_stage(bytes: &mut Vec<u8>, id: u32, prefix: &[u8]) {
    let length = prefix.len() as u64;
    stage_attributes(bytes, id, length);
    string(bytes, HANDLE, id + 1, b"verified");
    stage_attributes(bytes, id + 2, length);
    stage_attributes(bytes, id + 3, length);
    string(bytes, DATA, id + 4, prefix);
    stage_attributes(bytes, id + 5, length);
    stage_attributes(bytes, id + 6, length);
    status(bytes, id + 7, 0);
}

pub(super) fn window(
    context: &Context,
    f: impl FnOnce(&mut SftpWindow, &mpsc::Receiver<Command>, &Control),
) {
    window_with_policy(context, RemoteEditorPolicy::Deny, f);
}

pub(super) fn window_with_policy(
    context: &Context,
    policy: RemoteEditorPolicy,
    f: impl FnOnce(&mut SftpWindow, &mpsc::Receiver<Command>, &Control),
) {
    window_with_control(context, policy, control(context), f);
}

pub(super) fn window_without_verified_host(
    context: &Context,
    f: impl FnOnce(&mut SftpWindow, &mpsc::Receiver<Command>, &Control),
) {
    window_with_control(context, RemoteEditorPolicy::Deny, Control::default(), f);
}

fn window_with_control(
    context: &Context,
    policy: RemoteEditorPolicy,
    control: Control,
    f: impl FnOnce(&mut SftpWindow, &mpsc::Receiver<Command>, &Control),
) {
    let (commands, incoming) = mpsc::sync_channel(1);
    let (_finished, done) = bounded(1);
    let control = Arc::new(control);
    let mut window = SftpWindow::new(context.route.clone(), None, true, policy);
    window.worker = Some(Worker {
        commands,
        control: Arc::clone(&control),
        done,
        editor_policy: policy,
        exec: None,
        sftp: None,
    });
    window.configure_saved_downloads(Some(context.profile.clone()));
    f(&mut window, &incoming, &control);
}

fn complete_operation(
    window: &mut SftpWindow,
    control: &Control,
    command: Command,
    client: &mut Client,
) {
    let ticket = window.busy.as_ref().unwrap().id;
    let result =
        execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, result));
}

#[test]
fn sftp_window_reopens_an_upload_and_resolves_a_lost_publish_acknowledgement() {
    for lose_rename_ack in [false, true] {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let context = context(&root);
        let store = Store::new(&context.profile);
        let source = root.join("file.txt");
        fs::write(&source, b"first last").unwrap();
        window(&context, |window, incoming, control| {
            window
                .local
                .load(Arc::new(files::list_local(&root).unwrap()));
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                Vec::new(),
            )));
            let _ = window.update(WindowMessage::Select(
                Side::Local,
                EntryName::Local("file.txt".into()),
            ));
            let _ = window.update(WindowMessage::Transfer(Side::Local));
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::Copy { .. }));
            let mut interrupted = client(|bytes| {
                status(bytes, 1, 2); // Check selected destination.
                realpath(bytes, 2);
                status(bytes, 3, 2);
                string(bytes, HANDLE, 4, b"exclusive");
                for id in 5..=8 {
                    stage_attributes(bytes, id, 0);
                }
                // The first write has no acknowledgement. The server has accepted five bytes.
            });
            complete_operation(window, control, command, &mut interrupted);
            assert!(window.partial.as_ref().is_some_and(Offer::is_upload));
            assert!(store.uploads().unwrap().len() == 1);
        });

        window(&context, |window, incoming, control| {
            let _ = window.update(WindowMessage::FindInterruptedDownload);
            let mut reopened = client(|bytes| {
                realpath(bytes, 1);
                verify_stage(bytes, 2, b"first");
            });
            complete_operation(window, control, incoming.try_recv().unwrap(), &mut reopened);
            let offer = window.partial.as_ref().unwrap();
            assert!(offer.is_upload());
            assert_eq!(offer.offset, 5);
            let _ = window.update(WindowMessage::ResumeDownload);
            let mut resumed = client(|bytes| {
                realpath(bytes, 1);
                status(bytes, 2, 2);
                stage_attributes(bytes, 3, 5);
                string(bytes, HANDLE, 4, b"write");
                stage_attributes(bytes, 5, 5);
                stage_attributes(bytes, 6, 5);
                string(bytes, DATA, 7, b"first");
                stage_attributes(bytes, 8, 5);
                stage_attributes(bytes, 9, 5);
                status(bytes, 10, 0);
                stage_attributes(bytes, 11, 10);
                stage_attributes(bytes, 12, 10);
                status(bytes, 13, 0);
                realpath(bytes, 14);
                status(bytes, 15, 2);
                verify_stage(bytes, 16, b"first last");
                if !lose_rename_ack {
                    status(bytes, 24, 0);
                    realpath(bytes, 25);
                    string(bytes, HANDLE, 26, b"directory");
                    status(bytes, 27, 1);
                    status(bytes, 28, 0);
                }
            });
            complete_operation(window, control, incoming.try_recv().unwrap(), &mut resumed);
            if lose_rename_ack {
                assert!(window.partial.as_ref().is_some_and(Offer::is_upload));
                assert!(store.uploads().unwrap().len() == 1);
            } else {
                assert!(window.partial.is_none());
                assert!(window.status().unwrap().contains("Resumed file.txt"));
                assert!(store.uploads().unwrap().is_empty());
            }
        });

        if lose_rename_ack {
            window(&context, |window, incoming, control| {
                let _ = window.update(WindowMessage::FindInterruptedDownload);
                let mut resolved = client(|bytes| {
                    realpath(bytes, 1);
                    status(bytes, 2, 2); // The stage is gone because rename completed.
                    verify_stage(bytes, 3, b"first last");
                });
                complete_operation(window, control, incoming.try_recv().unwrap(), &mut resolved);
                assert!(window.partial.is_none());
                assert!(window.status().unwrap().contains("1 already published"));
                assert!(store.uploads().unwrap().is_empty());
            });
        }
        assert_eq!(fs::read(&source).unwrap(), b"first last");
    }
}
