//! A failed checkpoint contender must not alter the live owner's file access.

use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn sftp_resume_lock_contender_preserves_the_owners_final_permissions() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let partial = interrupt(&root.join("file.txt"));
    partial.persist_checkpoint(&context, || true).unwrap();
    drop(partial);
    let record = Store::new(&context.profile).list().unwrap().remove(0);
    let payload = root.join(&record.payload.stage_name).join("payload");
    let owner = Offer::recover(record.clone(), &context, || false).unwrap();
    {
        let state = owner.download_state().unwrap().lock().unwrap();
        let file = state.as_ref().unwrap().staged.writer().unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o751))
            .unwrap();
    }

    // This is the real receipt/reopen path. A contender can open the inode but
    // cannot acquire its transfer lock while the owner is finalizing it.
    let error = Offer::recover(record, &context, || false).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        fs::metadata(&payload).unwrap().permissions().mode() & 0o7777,
        0o751
    );
    assert_eq!(fs::read(&payload).unwrap(), b"first");

    {
        let state = owner.download_state().unwrap().lock().unwrap();
        let file = state.as_ref().unwrap().staged.writer().unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    owner.try_discard().unwrap();
    assert!(!payload.exists());
    assert!(Store::new(&context.profile).list().unwrap().is_empty());
}
