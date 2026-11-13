use super::*;

mod archive;
#[cfg(unix)]
mod archive_native;
mod browser_folders;
mod browser_listing;
mod browser_search;
pub(super) mod download;
mod file_compare;
mod folder_compare;
mod folder_queue;
#[cfg(unix)]
mod folder_sync_native;
#[cfg(unix)]
mod ownership_native;
#[cfg(unix)]
mod server_copy_native;
mod upload;
mod workbench;

#[test]
fn sftp_window_finds_and_discards_saved_downloads_only_outside_private_mode() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    drop(partial);

    let (commands, incoming) = mpsc::sync_channel(1);
    let (_finished, done) = bounded(1);
    let control = Arc::new(control(&context));
    let mut window = SftpWindow::new(context.route.clone(), None, true, RemoteEditorPolicy::Deny);
    window.worker = Some(Worker {
        commands,
        control: Arc::clone(&control),
        done,
        editor_policy: RemoteEditorPolicy::Deny,
        exec: None,
        sftp: None,
    });
    window.configure_saved_downloads(Some(context.profile.clone()));
    let _ = window.update(crate::sftp_window::Message::FindInterruptedDownload);
    let command = incoming.try_recv().unwrap();
    assert!(matches!(
        command.operation,
        Operation::FindInterruptedDownload
    ));
    let result = execute(
        &mut client(|_| {}),
        command.operation,
        &control,
        RemoteEditorPolicy::Deny,
    )
    .map(Arc::new);
    let ticket = window.busy.as_ref().unwrap().id;
    let _ = window.update(crate::sftp_window::Message::Done(ticket, result));
    assert!(window.partial.as_ref().is_some_and(Offer::available));
    assert!(!root.join("file.txt").exists());

    let directory = crate::private_files::Directory::open(&context.profile, false)
        .unwrap()
        .private_child("sftp-resume", false)
        .unwrap();
    let lock = directory.open_lock(".index.lock").unwrap();
    lock.lock().unwrap();
    let _ = window.update(crate::sftp_window::Message::DiscardPartial);
    assert!(window.partial.as_ref().is_some_and(Offer::available));
    assert!(window.status().unwrap().contains("Could not discard"));
    assert_eq!(Store::new(&context.profile).list().unwrap().len(), 1);
    drop(lock);

    let _ = window.update(crate::sftp_window::Message::DiscardPartial);
    assert!(window.partial.is_none());
    assert!(Store::new(&context.profile).list().unwrap().is_empty());
    window.set_incognito(true);
    assert!(storage::snapshot(&control).is_none());
    let _ = window.update(crate::sftp_window::Message::FindInterruptedDownload);
    assert!(incoming.try_recv().is_err());
}
