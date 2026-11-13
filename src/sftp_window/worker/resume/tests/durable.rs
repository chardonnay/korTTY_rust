use super::*;
use crate::sftp_window::worker::resume::durable::{self as storage, Context};
use crate::sftp_window::worker::resume::persist::Store;

mod native_upload;
#[cfg(unix)]
mod ownership;
mod ui;

fn context(root: &Path) -> Context {
    Context {
        profile: root.join("profile"),
        route: route("user"),
        host_key: HostKey::from_openssh(HOST).unwrap(),
    }
}

fn control(context: &Context) -> Control {
    Control {
        host_key: Some(context.host_key.clone()),
        resume: Mutex::new((1, Some(context.clone()))),
        ..Control::default()
    }
}

fn complete() -> Client {
    client(|bytes| {
        attributes(bytes, 1, 24);
        string(bytes, HANDLE, 2, b"source");
        attributes(bytes, 3, 24);
        attributes(bytes, 4, 24);
        string(bytes, DATA, 5, b"first");
        string(bytes, DATA, 6, b" last");
        status(bytes, 7, 1);
        attributes(bytes, 8, 24);
        status(bytes, 9, 0);
        attributes(bytes, 10, 24);
    })
}

#[test]
fn sftp_resume_survives_application_restart_and_clears_its_receipt_after_publication() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    let context = context(&root);
    let partial = interrupt(&destination);
    partial.persist_checkpoint(&context, || true).unwrap();
    drop(partial);
    assert!(!destination.exists());
    let reply = storage::find(&control(&context)).unwrap();
    let Reply::InterruptedDownload {
        offer: Some(offer), ..
    } = reply
    else {
        panic!("saved transfer must be available after reopening");
    };
    let progress = offer.progress();
    offer
        .run(&mut complete(), &progress, &control(&context))
        .unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"first last");
    assert!(Store::new(&context.profile).list().unwrap().is_empty());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
}

#[test]
fn sftp_resume_rejects_another_route_and_exclusive_ownership() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    // A second process cannot reopen the still-owned stage.
    assert!(matches!(
        storage::find(&control(&context)).unwrap(),
        Reply::InterruptedDownload { offer: None, .. }
    ));
    drop(partial);
    let mut other = context.clone();
    other.route = route("other");
    assert!(matches!(
        storage::find(&control(&other)).unwrap(),
        Reply::InterruptedDownload { offer: None, .. }
    ));
    other = context.clone();
    other.host_key = HostKey::from_openssh(ROTATED).unwrap();
    assert!(matches!(
        storage::find(&control(&other)).unwrap(),
        Reply::InterruptedDownload { offer: None, .. }
    ));
    let Reply::InterruptedDownload {
        offer: Some(offer), ..
    } = storage::find(&control(&context)).unwrap()
    else {
        panic!("original route can reopen");
    };
    offer.discard();
    assert!(Store::new(&context.profile).list().unwrap().is_empty());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn released_download_unlocks_a_cloned_file_description() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    let download = partial.take().unwrap();
    let inherited = download.staged.writer().unwrap().try_clone().unwrap();
    drop(download);
    let Reply::InterruptedDownload {
        offer: Some(offer), ..
    } = storage::find(&control(&context)).unwrap()
    else {
        panic!("the released payload must unlock its cloned file description");
    };
    drop(inherited);
    offer.try_discard().unwrap();
}

#[test]
fn sftp_resume_rejects_changed_prefix_without_deleting_the_unverified_payload() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    let record = Store::new(&context.profile).list().unwrap().remove(0);
    drop(partial);
    let payload = root.join(&record.payload.stage_name).join("payload");
    fs::write(&payload, b"other").unwrap();
    assert!(Offer::recover(record, &context, || false).is_err());
    assert_eq!(fs::read(&payload).unwrap(), b"other");
}

#[test]
fn sftp_resume_truncates_only_the_uncommitted_suffix_after_a_crash() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    let record = Store::new(&context.profile).list().unwrap().remove(0);
    drop(partial);
    let payload = root.join(&record.payload.stage_name).join("payload");
    fs::OpenOptions::new()
        .append(true)
        .open(&payload)
        .unwrap()
        .write_all(b"partial suffix")
        .unwrap();
    let offer = Offer::recover(record, &context, || false).unwrap();
    assert_eq!(fs::metadata(&payload).unwrap().len(), 5);
    offer.discard();
    assert!(!payload.exists());
}

#[test]
fn sftp_resume_does_not_persist_without_an_unchanged_approved_context() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    assert!(partial.persist_checkpoint(&context, || false).is_err());
    assert!(!context.profile.exists());
    partial.discard();
    assert!(fs::read_dir(&root).unwrap().next().is_none());
}

#[test]
fn sftp_resume_reconciles_a_published_file_after_interrupted_receipt_cleanup() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    let context = context(&root);
    let partial = interrupt(&destination);
    partial.persist_checkpoint(&context, || true).unwrap();
    let download = partial.take().unwrap();
    let mut writer = download.staged.writer().unwrap();
    writer.seek(SeekFrom::End(0)).unwrap();
    writer.write_all(b" last").unwrap();
    download.staged.publish(|| false).unwrap();
    assert_eq!(Store::new(&context.profile).list().unwrap().len(), 1);
    let reply = storage::find(&control(&context)).unwrap();
    assert!(matches!(
        reply,
        Reply::InterruptedDownload { offer: None, .. }
    ));
    assert!(Store::new(&context.profile).list().unwrap().is_empty());
    assert_eq!(fs::read(destination).unwrap(), b"first last");
}

#[test]
fn sftp_resume_preserves_an_unrelated_destination_and_rejects_a_changed_remote() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let destination = root.join("file.txt");
    let context = context(&root);
    let partial = interrupt(&destination);
    partial.persist_checkpoint(&context, || true).unwrap();
    drop(partial);
    fs::write(&destination, b"other data").unwrap();
    let reply = storage::find(&control(&context)).unwrap();
    assert!(matches!(
        reply,
        Reply::InterruptedDownload { offer: None, .. }
    ));
    assert_eq!(fs::read(&destination).unwrap(), b"other data");
    assert_eq!(Store::new(&context.profile).list().unwrap().len(), 1);
    fs::remove_file(&destination).unwrap();
    let Reply::InterruptedDownload {
        offer: Some(offer), ..
    } = storage::find(&control(&context)).unwrap()
    else {
        panic!("original private payload remains available");
    };
    let progress = offer.progress();
    let error = offer
        .run(
            &mut client(|bytes| attributes(bytes, 1, 25)),
            &progress,
            &control(&context),
        )
        .unwrap_err();
    assert!(error.message.contains("changed"));
    assert!(Store::new(&context.profile).list().unwrap().is_empty());
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn sftp_resume_forgets_deleted_stages_but_keeps_unavailable_parent_receipts() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    let record = Store::new(&context.profile).list().unwrap().remove(0);
    drop(partial);
    let stage = root.join(&record.payload.stage_name);
    fs::remove_file(stage.join("payload")).unwrap();
    fs::remove_dir(&stage).unwrap();
    assert!(matches!(
        storage::find(&control(&context)).unwrap(),
        Reply::InterruptedDownload { offer: None, .. }
    ));
    assert!(Store::new(&context.profile).list().unwrap().is_empty());

    let parent = root.join("downloads");
    fs::create_dir(&parent).unwrap();
    let partial = interrupt(&parent.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    drop(partial);
    fs::rename(&parent, root.join("unavailable")).unwrap();
    assert!(matches!(
        storage::find(&control(&context)).unwrap(),
        Reply::InterruptedDownload { offer: None, .. }
    ));
    assert_eq!(Store::new(&context.profile).list().unwrap().len(), 1);
}
