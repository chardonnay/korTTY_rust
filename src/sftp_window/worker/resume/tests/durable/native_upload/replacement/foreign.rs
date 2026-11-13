//! Opt-in foreign-owner verification with an externally seeded synthetic file.
//!
//! The fixture directory and its two pre-existing entries belong to the
//! caller. This test removes only the new private backup directories that
//! it observes the application create over the pinned connection.

use super::*;

const TARGET: &[u8] = b"foreign.bin";
const ALIAS: &[u8] = b"foreign-link.bin";
const REPLACEMENT: &[u8] = b"verified replacement\n";

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and pre-seeded FOREIGN_SCRATCH"]
fn live_sftp_reviewed_foreign_owner_replacement_preserves_shared_inode() {
    let fixture = SshFixture::read("FOREIGN_SCRATCH", b"kortty-foreign-live-");
    let (_, name) = remote_components(&fixture.scratch).unwrap();
    let nonce = name.strip_prefix(b"kortty-foreign-live-").unwrap();
    let nonce = std::str::from_utf8(nonce).unwrap();
    let original = format!("kortty foreign-owner test {nonce}\n");
    let original = original.as_bytes();

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let local = root.join("files");
    fs::create_dir(&local).unwrap();
    let local_source = local.join("foreign.bin");
    write_local(&local_source, REPLACEMENT, MODIFIED + 10);
    let context = fixture.context(root.join("profile"));
    let trace = Arc::new(Trace::new(fixture.scratch.clone()));
    let (mut session, mut client) = traced_client(&fixture, &trace);
    let folder = client.lstat(&fixture.scratch).unwrap();
    let owner = folder.uid.zip(folder.gid).expect("known fixture owner");
    require_folder(&mut client, &fixture.scratch, owner).unwrap();

    let entries = client.read_dir_limited(&fixture.scratch, 4, 4096).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry.name == TARGET || entry.name == ALIAS)
    );
    let target = files::remote_join(&fixture.scratch, TARGET);
    let alias = files::remote_join(&fixture.scratch, ALIAS);
    let stamp = client.lstat(&target).unwrap();
    let foreign_owner = stamp.uid.expect("known foreign owner");
    assert_ne!(foreign_owner, owner.0);
    assert_ne!(foreign_owner, u32::MAX);
    assert_eq!(stamp.gid, Some(owner.1));
    for path in [&target, &alias] {
        assert_eq!(client.canonicalize(path).unwrap(), *path);
        let current = client.lstat(path).unwrap();
        assert_eq!(current.kind(), FileKind::File);
        assert_eq!(current.uid, Some(foreign_owner));
        assert_eq!(current.gid, Some(owner.1));
        assert_eq!(
            read_remote(&mut client, path),
            (original.to_vec(), 0o660, MODIFIED),
            "the protected fixture must contain the exact synthetic original"
        );
    }

    let mut recovery = Recovery {
        fixture: &fixture,
        trace: Arc::clone(&trace),
        owner,
        cleaned: false,
    };
    let report = run_review(
        &context,
        &mut client,
        &local,
        &fixture.scratch,
        Side::Remote,
        ReviewAction::Foreign,
    );
    assert!(
        report.contains("Replaced the reviewed server file"),
        "{report}"
    );
    let writes = trace.writes.lock().unwrap().clone();
    assert!(writes.moves.is_empty() && writes.links.is_empty() && writes.removals.is_empty());
    assert_eq!(writes.stages.len(), 1);
    let backup = files::remote_join(&writes.stages[0], b"payload");
    assert!(
        report.contains(&backup.escape_ascii().to_string()),
        "{report}"
    );
    let backup_stamp = client.lstat(&backup).unwrap();
    assert_eq!(backup_stamp.uid, Some(owner.0));
    let (original_saved, backup_mode, _) = read_remote(&mut client, &backup);
    assert_eq!(backup_mode, 0o600);
    assert_eq!(hash(&original_saved), hash(original));

    let (bytes, mode, modified) = read_remote(&mut client, &target);
    assert_eq!(bytes, REPLACEMENT);
    assert_eq!(mode, 0o660);
    assert_eq!(read_remote(&mut client, &alias), (bytes, mode, modified));
    for path in [&target, &alias] {
        let attrs = client.lstat(path).unwrap();
        assert_eq!(attrs.uid, Some(foreign_owner));
        assert_eq!(attrs.gid, Some(owner.1));
    }
    assert_eq!(
        report.contains("The server did not preserve the source modification time"),
        modified != MODIFIED + 10,
        "{report}"
    );

    recovery.cleanup(&mut client).unwrap();
    assert!(client.lstat(&writes.stages[0]).unwrap_err().is_not_found());
    assert_eq!(client.read_dir(&fixture.scratch).unwrap().len(), 2);
    drop(client);
    session.close().unwrap();
    println!(
        "OpenSSH verified an unchanged foreign UID/group and hard-link alias, original backup, and source mtime {}",
        if modified == MODIFIED + 10 {
            "preserved"
        } else {
            "refused"
        }
    );
}

struct Recovery<'a> {
    fixture: &'a SshFixture,
    trace: Arc<Trace>,
    owner: (u32, u32),
    cleaned: bool,
}

impl Recovery<'_> {
    fn cleanup(&mut self, client: &mut Client) -> std::result::Result<(), String> {
        require_folder(client, &self.fixture.scratch, self.owner)?;
        self.trace.cleanup(client, self.owner)?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for Recovery<'_> {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let result = self
            .fixture
            .open(Arc::new(Probe::new(Cut::None)))
            .and_then(|(_session, mut client)| self.cleanup(&mut client));
        if let Err(error) = result {
            eprintln!("Could not remove the private foreign-owner backup fixture: {error}");
        }
    }
}

fn require_folder(
    client: &mut Client,
    path: &[u8],
    owner: (u32, u32),
) -> std::result::Result<(), String> {
    let attrs = client.lstat(path).map_err(|error| error.to_string())?;
    if attrs.kind() != FileKind::Directory
        || attrs.uid.zip(attrs.gid) != Some(owner)
        || attrs.permissions.map(|mode| mode & 0o7777) != Some(0o700)
        || client
            .canonicalize(path)
            .map_err(|error| error.to_string())?
            != path
    {
        return Err(
            "the foreign-owner fixture directory is no longer canonical and private".into(),
        );
    }
    Ok(())
}
