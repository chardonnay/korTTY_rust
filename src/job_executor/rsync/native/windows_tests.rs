use std::collections::BTreeSet;
use std::fs;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use kortty_jobs::{
    Job, JobGraph, JobId, JobSchedule, RunTrigger, Scheduler, SchedulerStore, TimeZone,
};
use kortty_server_archive::rsync_tree::DeleteScope;
use kortty_server_archive::rsync_tree::projection::{CollisionKind, Placement};
use kortty_ssh::{Authentication, Endpoint, HostKey};
use serde_json::json;

use crate::connection_store::{Auth, Connection, SavedLogin, Store};
use crate::scheduler_window::rsync::{
    MixedDestination, MixedReceiver, MixedReviewTarget, standing,
};
use crate::scheduler_window::{ConnectionRef, Message, SchedulerView, SchedulerWindow};
use crate::ssh_dialog::LoginTarget;

use super::*;

const SERVER_BYTES: &[u8] = b"kortty Windows rsync fixture\n";

#[test]
fn windows_rsync_scratch_discards_readonly_directories_and_files() {
    let profile = crate::snippets::tests::Profile::new();
    let stage = Scratch::new(&profile.0).unwrap();
    let path = stage.path.clone();
    let directory = path.join("payload");
    let file = directory.join("data");
    fs::create_dir(&directory).unwrap();
    fs::write(&file, b"read-only rsync data").unwrap();
    for entry in [&file, &directory] {
        let mut permissions = fs::metadata(entry).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(entry, permissions).unwrap();
    }

    drop(stage);
    assert!(!path.exists(), "raw rsync stage remained at {path:?}");
}

#[test]
fn windows_rsync_program_review_checks_a_pe_image_and_held_path() {
    let profile = crate::snippets::tests::Profile::new();
    let program = profile.0.join("rsync.exe");
    fs::write(&program, b"an executable-looking filename").unwrap();
    assert!(program_digest(&program).is_err());
    let mut image = [0_u8; 90];
    image[..2].copy_from_slice(b"MZ");
    image[60] = 64;
    image[64..68].copy_from_slice(b"PE\0\0");
    image[68..70].copy_from_slice(&0x8664_u16.to_le_bytes());
    image[86..88].copy_from_slice(&2_u16.to_le_bytes());
    image[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
    fs::write(&program, image).unwrap();
    assert_eq!(
        program_digest(&program).unwrap(),
        program_digest(&program.canonicalize().unwrap()).unwrap()
    );
    let script = profile.0.join("rsync.cmd");
    fs::write(&script, image).unwrap();
    assert!(program_digest(&script).is_err());
    let library = profile.0.join("runtime.dll");
    fs::write(&library, image).unwrap();
    let reviewed = program_digest(&program).unwrap();
    let sidecar = profile.0.join("rsync.exe.local");
    fs::write(&sidecar, b"unreviewed sidecar").unwrap();
    assert_eq!(program_digest(&program).unwrap(), reviewed);
    let prepared = windows_program::Prepared::new(&program, reviewed, &|| false).unwrap();
    let staged = prepared.program().parent().unwrap();
    assert_eq!(fs::read(staged.join("runtime.dll")).unwrap(), image);
    assert!(!staged.join("rsync.exe.local").exists());
    assert!(fs::write(staged.join("runtime.dll"), b"replacement").is_err());
    image[74] ^= 1;
    fs::write(library, image).unwrap();
    assert_ne!(program_digest(&program).unwrap(), reviewed);
    assert!(windows_program::Prepared::new(&program, reviewed, &|| false).is_err());
    let removed = staged.to_owned();
    drop(prepared);
    assert!(!removed.exists());
}

#[test]
#[ignore = "requires a reviewed Windows rsync.exe and the native kortty executable"]
fn windows_rsync_client_runs_the_native_bridge_with_literal_arguments() {
    let executable = program();
    let reviewed = program_digest(&executable).unwrap();
    let bundle = windows_program::Prepared::new(&executable, reviewed, &|| false).unwrap();
    let executable = bundle.program();
    process::check_version(executable, &|| false).unwrap();
    let profile = crate::snippets::tests::Profile::new();
    let scratch = Scratch::new(&profile.0).unwrap();
    let (listener, endpoint) = bridge::bind(&scratch.path).unwrap();
    listener.set_nonblocking(true).unwrap();
    // A local process outside the rsync Job may connect before the helper.
    let unrelated = bridge::Stream::connect(listener.local_addr().unwrap()).unwrap();
    let token = nonce().unwrap();
    let remote = "/fixture/path-to-file";
    let mut client = process::Process::start(
        Command::new(executable)
            .current_dir(&scratch.path)
            .env("LANG", "C")
            .env(bridge::TOKEN_ENV, &token)
            .env(bridge::OWNER_ENV, std::process::id().to_string())
            .env("MSYS2_ARG_CONV_EXCL", "*")
            .args([
                "--max-size=268435456",
                "--no-links",
                "--no-specials",
                "--no-devices",
                "--no-relative",
                "--no-recursive",
                "-e",
            ])
            .arg(bridge::shell(&bridge_executable().unwrap(), &endpoint).unwrap())
            .arg("--")
            .args([format!("{}:{remote}", bridge::HOST), format!("./{PAYLOAD}")]),
    )
    .unwrap();
    let mut stream = accept(&listener, &mut client, &|| false).unwrap();
    let (command, arguments) =
        bridge::command(&mut stream, &token, remote, bridge::Mode::File, &|| false).unwrap();
    assert!(command.ends_with(" . '/fixture/path-to-file'"), "{command}");
    assert!(arguments.is_empty());
    drop(unrelated);
}

#[test]
#[ignore = "requires a Windows rsync client and a disposable pinned OpenSSH fixture"]
fn windows_rsync_pinned_ssh_downloads_and_uploads_verified_files() {
    let fixture = Fixture::read();
    let profile = crate::snippets::tests::Profile::new();
    let local = profile.0.canonicalize().unwrap();
    let download = local.join("from server.bin");
    fixture.transfer(&download, "download", false, false);
    assert_eq!(fs::read(download).unwrap(), SERVER_BYTES);

    let source = local.join("for upload.bin");
    let expected: Vec<u8> = (0..196_610).map(|index| (index % 251) as u8).collect();
    fs::write(&source, &expected).unwrap();
    fixture.transfer(&source, "upload", true, false);
    let copied = local.join("read back.bin");
    fixture.transfer(&copied, "upload", false, false);
    assert_eq!(fs::read(copied).unwrap(), expected);
}

#[test]
#[ignore = "requires a Windows rsync client and a disposable pinned OpenSSH fixture with the current archive helper"]
fn windows_rsync_pinned_ssh_downloads_and_uploads_verified_directories() {
    let fixture = Fixture::read();
    let profile = crate::snippets::tests::Profile::new();
    let local = profile.0.canonicalize().unwrap();
    let download = local.join("download tree");
    fixture.transfer(&download, "download-tree", false, true);
    assert!(download.join("empty").is_dir());
    assert_eq!(
        fs::read(download.join("nested").join("content")).unwrap(),
        SERVER_BYTES
    );
    fixture.transfer(&download, "upload-tree", true, true);
    let copied = local.join("read back tree");
    fixture.transfer(&copied, "upload-tree", false, true);
    assert!(copied.join("empty").is_dir());
    assert_eq!(
        fs::read(copied.join("nested").join("content")).unwrap(),
        SERVER_BYTES
    );
}

#[test]
#[ignore = "requires native Windows rsync, the current bridge, and a disposable pinned OpenSSH source tree"]
fn windows_rsync_pinned_ssh_replaces_only_the_reviewed_local_directory() {
    let fixture = Fixture::read();
    for named in [false, true] {
        fixture.existing_download(named);
    }
}

#[test]
#[ignore = "requires native Windows rsync, the current bridge, and a disposable pinned OpenSSH source file"]
fn windows_rsync_pinned_ssh_replaces_only_the_reviewed_local_file() {
    Fixture::read().existing_file_download();
}

#[test]
#[ignore = "requires native Windows rsync, the current bridge, and a disposable pinned OpenSSH source tree"]
fn windows_rsync_pinned_ssh_projects_one_mixed_local_directory() {
    let fixture = Fixture::read();
    for existing in [false, true] {
        fixture.mixed_download(existing);
    }
}

#[test]
#[ignore = "requires Windows rsync, the current bridge, and the disposable pinned OpenSSH download-tree fixture"]
fn windows_recurring_rsync_advances_the_saved_receiver_and_blocks_tampering() {
    let before = raw_scratch_paths();
    Fixture::read().recurring_download();
    let after = raw_scratch_paths();
    let leftover: Vec<_> = after.difference(&before).collect();
    assert!(leftover.is_empty(), "raw rsync stages remain: {leftover:?}");
}

fn raw_scratch_paths() -> BTreeSet<PathBuf> {
    fs::read_dir(std::env::temp_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("kr-") && name.len() == 25)
        })
        .collect()
}

fn program() -> PathBuf {
    PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").expect("Windows rsync executable"))
        .canonicalize()
        .unwrap()
}

struct Fixture {
    program: PathBuf,
    endpoint: Endpoint,
    username: String,
    key_file: PathBuf,
    trust: TrustStore,
    remote_root: String,
}

impl Fixture {
    fn read() -> Self {
        let required = |variable| std::env::var(variable).unwrap();
        let address: std::net::SocketAddr =
            required("KORTTY_RSYNC_TEST_SSH_ADDRESS").parse().unwrap();
        let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
        let host_key = HostKey::from_openssh(&required("KORTTY_RSYNC_TEST_HOST_KEY")).unwrap();
        let trust = TrustStore::from_pins(vec![(endpoint.clone(), host_key)]).unwrap();
        let remote_root = required("KORTTY_RSYNC_TEST_REMOTE_ROOT");
        assert!(rsync::valid_remote(&remote_root));
        assert!(
            remote_root
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with("kortty-rsync-fixture-"))
        );
        Self {
            program: program(),
            endpoint,
            username: required("KORTTY_RSYNC_TEST_SSH_USER"),
            key_file: required("KORTTY_RSYNC_TEST_SSH_KEY").into(),
            trust,
            remote_root,
        }
    }

    fn transfer(&self, local: &Path, name: &str, upload: bool, directory: bool) {
        let reference: crate::scheduler_window::ConnectionRef = serde_json::from_value(json!({
            "id": 1,
            "target": {
                "host": self.endpoint.host(),
                "port": self.endpoint.port(),
                "username": self.username,
            },
        }))
        .unwrap();
        let remote = format!("{}/{name}", self.remote_root);
        let pins = sync::fingerprints(&sync::pins(&reference, &self.trust).unwrap());
        let digest = program_digest(&self.program).unwrap();
        let mut approval = json!({
            "program": self.program, "digest": digest, "reference": reference, "pins": pins,
        });
        if upload && directory {
            let inventory = upload_tree_inventory(local).unwrap();
            approval["directory_upload"] =
                json!({"source": local, "destination": remote, "inventory": inventory});
        } else if upload {
            let (bytes, digest) = upload_digest(local).unwrap();
            approval["upload"] =
                json!({"source": local, "destination": remote, "bytes": bytes, "digest": digest});
        } else {
            approval[if directory { "directory" } else { "download" }] =
                json!({"source": remote, "destination": local});
        }
        let definition: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {
                "kind": if upload {"rsync_upload"} else {"rsync_download"},
                "program": self.program, "local": local, "remote": remote,
                "approval": approval, "directory": directory,
            },
        }))
        .unwrap();
        let id = JobId::new("windows-rsync-fixture").unwrap();
        let graph = JobGraph::new(vec![
            Job::new(
                id.clone(),
                "Windows rsync transfer",
                definition.validated().unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let mut scheduler = Scheduler::new(graph);
        let request = scheduler
            .start_manual(&id, SystemTime::now())
            .unwrap()
            .started
            .pop()
            .unwrap();
        let action = request.action.remote_action().unwrap();
        let (transfer, _) = action.rsync_transfer().unwrap();
        let require_target = || SshTarget {
            endpoint: self.endpoint.clone(),
            username: self.username.clone(),
            authentication: Authentication::private_key(fs::read(&self.key_file).unwrap()),
        };
        let policy = ExecutionPolicy::unrestricted(true);
        let missing = TrustStore::from_pins(Vec::new()).unwrap();
        let blocked = run(
            &request,
            transfer,
            upload,
            &policy,
            require_target(),
            Vec::new(),
            &missing,
        );
        assert!(matches!(
            blocked,
            Err(Failure {
                outcome: RunOutcome::Blocked,
                ..
            })
        ));
        let result = run(
            &request,
            transfer,
            upload,
            &policy,
            require_target(),
            Vec::new(),
            &self.trust,
        )
        .unwrap_or_else(|failure| panic!("{}", failure.detail));
        assert_eq!(result.outcome, RunOutcome::Success);
    }

    fn existing_download(&self, named: bool) {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = local_tree::Directory::open_path(&root).unwrap();
        let owned = OsStr::new("protected");
        drop(local_tree::create_reviewed_tree_stage(&directory, owned).unwrap());
        let local = root.join(owned);
        let configured = local.join("target");
        let effective = if named {
            configured.join("download-tree")
        } else {
            configured.clone()
        };
        fs::create_dir_all(effective.join("nested")).unwrap();
        let replaces = effective.join("nested/content");
        fs::write(&replaces, b"old Windows receiver").unwrap();
        fs::write(effective.join("local-only"), b"receiver-only payload").unwrap();
        if named {
            fs::write(configured.join("sibling"), b"outside reviewed receiver").unwrap();
        }
        let access = |path: &Path| {
            kortty_files::ReplacementMetadata::capture(&File::open(path).unwrap()).unwrap()
        };
        let original_access = access(&replaces);
        let original_identity = local_tree::Directory::open_path(&effective)
            .unwrap()
            .identity()
            .unwrap();
        let snapshot = local_existing::review(&effective, &AtomicBool::new(false)).unwrap();
        let (connections, reference) = self.save_connection(&root);
        let remote = format!("{}/download-tree", self.remote_root);
        let settings = json!({ "delete": named, "include_source_name": named });
        let pins = sync::fingerprints(&sync::pins(&reference, &self.trust).unwrap());
        let digest = program_digest(&self.program).unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {
                "kind": "rsync_download", "directory": true, "program": self.program,
                "local": configured, "remote": remote, "existing_download": settings,
                "approval": {
                    "program": self.program, "digest": digest, "reference": reference, "pins": pins,
                    "directory": { "source": remote, "destination": configured },
                    "existing_download": { "destination": effective, "snapshot": snapshot, "settings": settings },
                },
            },
        })).unwrap();
        let mut scheduler = saved_scheduler(&root, &connections, action);
        let mut view = scheduler.new_view();
        let result = self.run_saved(&mut scheduler, &mut view);
        assert_eq!(result.outcome, RunOutcome::Success, "{result:?}");
        assert_eq!(fs::read(&replaces).unwrap(), SERVER_BYTES);
        assert_eq!(access(&replaces), original_access);
        assert!(effective.join("empty").is_dir());
        assert_eq!(effective.join("local-only").exists(), !named);
        if named {
            assert_eq!(
                fs::read(configured.join("sibling")).unwrap(),
                b"outside reviewed receiver"
            );
        }
        let recovered = recovery_paths(&effective);
        assert_eq!(recovered.len(), 1, "{result:?}");
        assert!(
            result
                .detail
                .as_deref()
                .unwrap()
                .contains(recovered[0].to_str().unwrap())
        );
        assert_eq!(
            fs::read(recovered[0].join("nested/content")).unwrap(),
            b"old Windows receiver"
        );
        assert_eq!(
            fs::read(recovered[0].join("local-only")).unwrap(),
            b"receiver-only payload"
        );
        assert_eq!(
            local_tree::Directory::open_path(&recovered[0])
                .unwrap()
                .identity()
                .unwrap(),
            original_identity
        );
        let replay = self.run_saved(&mut scheduler, &mut view);
        assert_eq!(replay.outcome, RunOutcome::Blocked, "{replay:?}");
        assert_eq!(fs::read(replaces).unwrap(), SERVER_BYTES);
        assert_eq!(recovery_paths(&effective), recovered);
    }

    fn existing_file_download(&self) {
        use std::os::windows::fs::MetadataExt as _;

        use crate::sftp_window::ReviewedFileReceiver;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = local_tree::Directory::open_path(&root).unwrap();
        let workspace = OsStr::new("protected");
        drop(local_tree::create_reviewed_tree_stage(&directory, workspace).unwrap());
        let destination = root.join(workspace).join("existing reviewed file.bin");
        let original = b"original Windows file receiver";
        fs::write(&destination, original).unwrap();
        let inspect = |path: &Path| ReviewedFileReceiver::inspect(path, &|| false).unwrap();
        let metadata = |path: &Path| {
            let file = File::open(path).unwrap();
            let info = file.metadata().unwrap();
            (
                kortty_files::ReplacementMetadata::capture(&file).unwrap(),
                info.creation_time(),
                info.last_write_time(),
                info.file_attributes(),
            )
        };
        let original_metadata = metadata(&destination);
        let snapshot = inspect(&destination);
        let (connections, reference) = self.save_connection(&root);
        let source = format!("{}/download", self.remote_root);
        let pins = sync::fingerprints(&sync::pins(&reference, &self.trust).unwrap());
        let digest = program_digest(&self.program).unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {
                "kind": "rsync_download", "directory": false, "program": self.program,
                "local": destination, "remote": source,
                "existing_download": {"include_source_name": false, "delete": false},
                "approval": {
                    "program": self.program, "digest": digest, "reference": reference, "pins": pins,
                    "download": {"source": source, "destination": destination},
                    "existing_file_download": {"destination": destination, "snapshot": snapshot},
                },
            },
        }))
        .unwrap();
        let mut scheduler = saved_scheduler(&root, &connections, action);
        let mut view = scheduler.new_view();
        let result = self.run_saved(&mut scheduler, &mut view);
        assert_eq!(result.outcome, RunOutcome::Success, "{result:?}");
        let successor = inspect(&destination);
        assert!(!successor.aliases(&snapshot));
        assert_eq!(fs::read(&destination).unwrap(), SERVER_BYTES);
        assert_eq!(metadata(&destination), original_metadata);

        let recovery_files = || {
            recovery_paths(&destination)
                .into_iter()
                .map(|path| path.with_file_name(destination.file_name().unwrap()))
                .collect::<Vec<_>>()
        };
        let recovered = recovery_files();
        assert_eq!(recovered.len(), 1, "{result:?}");
        let recovery = &recovered[0];
        assert!(inspect(recovery).aliases(&snapshot));
        assert_eq!(fs::read(recovery).unwrap(), original);
        assert_eq!(metadata(recovery), original_metadata);
        assert!(
            result
                .detail
                .as_deref()
                .unwrap()
                .contains(recovery.to_str().unwrap())
        );

        let replay = self.run_saved(&mut scheduler, &mut view);
        assert_eq!(replay.outcome, RunOutcome::Blocked, "{replay:?}");
        assert_eq!(inspect(&destination), successor);
        assert_eq!(recovery_files(), recovered);
    }

    fn mixed_download(&self, existing: bool) {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = local_tree::Directory::open_path(&root).unwrap();
        let owned = OsStr::new("protected");
        drop(local_tree::create_reviewed_tree_stage(&directory, owned).unwrap());
        let destination = root.join(owned).join("mixed target");
        let old = destination.join("download-tree/old");
        if existing {
            fs::create_dir_all(old.parent().unwrap()).unwrap();
            fs::write(&old, b"reviewed old content").unwrap();
            fs::write(destination.join("keep"), b"outside named mirror").unwrap();
        }
        let primary = format!("{}/download", self.remote_root);
        let tree = format!("{}/download-tree", self.remote_root);
        let mut sources = vec![
            primary.clone(),
            tree.clone(),
            format!("{tree}/nested/content"),
        ];
        if !existing {
            // The direct file and the contents directory collide at "content".
            sources.push(format!("{tree}/nested/"));
        }
        let (connections, reference) = self.save_connection(&root);
        let target = MixedReviewTarget {
            reference: reference.clone(),
            sources: sources.clone(),
            destination: MixedDestination::Local(destination.clone()),
            delete: existing,
            elevation: crate::scheduler_window::rsync::Elevation::Ordinary,
        };
        let credentials = crate::job_executor::Credentials::resolve_directory_browser(
            &reference,
            connections.get(reference.id).unwrap(),
            &crate::clipboard_policy::ServerAccess::default(),
            |_, _, _| Err("Unexpected saved password in the key-only rsync fixture."),
        )
        .unwrap();
        let review = crate::job_executor::rsync::preview_mixed(
            credentials,
            &target,
            &self.trust,
            Arc::new(AtomicBool::new(false)),
            || Ok(()),
        )
        .unwrap();
        assert!(matches!(
            &review.receiver,
            MixedReceiver::Local { destination: checked, snapshot }
                if checked == &destination && snapshot.is_some() == existing
        ));
        assert_eq!(
            review.plan.delete,
            if existing {
                DeleteScope::Named(vec!["download-tree".into()])
            } else {
                DeleteScope::Retain
            }
        );
        assert_eq!(
            review.plan.sources[0].placement,
            Placement::File("download".into())
        );
        assert_eq!(
            review.plan.sources[1].placement,
            Placement::Named("download-tree".into())
        );
        if existing {
            assert_eq!(review.plan.collision_count, 0);
        } else {
            assert_eq!(review.plan.sources[3].placement, Placement::Contents);
            assert_eq!(review.plan.collision_count, 1);
            let collision = &review.plan.collisions[0];
            assert_eq!(collision.relative, "content");
            assert_eq!((collision.winner, collision.loser), (2, 3));
            assert_eq!(collision.kind, CollisionKind::EarlierFile);
        }
        let pins = sync::fingerprints(&sync::pins(&reference, &self.trust).unwrap());
        let digest = program_digest(&self.program).unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {
                "kind": "rsync_download", "directory": true, "program": self.program,
                "local": destination, "remote": primary, "mixed": sources,
                "existing_download": { "delete": existing, "include_source_name": false },
                "approval": {
                    "program": self.program, "digest": digest, "reference": reference, "pins": pins,
                    "mixed": review,
                },
            },
        }))
        .unwrap();
        let mut scheduler = saved_scheduler(&root, &connections, action);
        let mut view = scheduler.new_view();
        let result = self.run_saved(&mut scheduler, &mut view);
        assert_eq!(result.outcome, RunOutcome::Success, "{result:?}");
        assert_eq!(
            fs::read(destination.join("download")).unwrap(),
            SERVER_BYTES
        );
        assert_eq!(fs::read(destination.join("content")).unwrap(), SERVER_BYTES);
        assert_eq!(
            fs::read(destination.join("download-tree/nested/content")).unwrap(),
            SERVER_BYTES
        );
        assert!(destination.join("download-tree/empty").is_dir());
        let recovered = recovery_paths(&destination);
        assert_eq!(recovered.len(), usize::from(existing), "{result:?}");
        if existing {
            assert!(!old.exists());
            assert_eq!(
                fs::read(destination.join("keep")).unwrap(),
                b"outside named mirror"
            );
            assert_eq!(
                fs::read(recovered[0].join("download-tree/old")).unwrap(),
                b"reviewed old content"
            );
            assert!(
                result
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains(recovered[0].to_str().unwrap())
            );
        }
        let replay = self.run_saved(&mut scheduler, &mut view);
        assert_eq!(replay.outcome, RunOutcome::Blocked, "{replay:?}");
        assert_eq!(recovery_paths(&destination), recovered);
    }

    fn recurring_download(&self) {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = local_tree::Directory::open_path(&root).unwrap();
        let workspace = OsStr::new("protected");
        drop(local_tree::create_reviewed_tree_stage(&directory, workspace).unwrap());
        let destination = root.join(workspace).join("recurring receiver");
        fs::create_dir_all(destination.join("nested")).unwrap();
        let content = destination.join("nested/content");
        fs::write(&content, b"original Windows receiver").unwrap();
        fs::write(destination.join("obsolete"), b"one mirror deletion").unwrap();
        let initial = local_existing::inspect(&destination, &|| false).unwrap();
        let (connections, reference) = self.save_connection(&root);
        let remote = format!("{}/download-tree", self.remote_root);
        let pins = sync::fingerprints(&sync::pins(&reference, &self.trust).unwrap());
        let digest = program_digest(&self.program).unwrap();
        let settings = json!({ "delete": true, "include_source_name": false });
        let mut transfer = json!({
            "kind": "rsync_download", "directory": true, "program": self.program,
            "local": destination, "remote": remote, "existing_download": settings,
            "approval": {
                "program": self.program, "digest": digest, "reference": reference, "pins": pins,
                "directory": { "source": remote, "destination": destination },
                "existing_download": { "destination": destination, "snapshot": initial, "settings": settings },
            },
            "standing": { "settings": {
                "days": 2, "firings": 3, "budget": { "bytes": 4096, "entries": 32, "deletes": 1 }
            }},
        });
        let id = JobId::new("windows-recurring-rsync").unwrap();
        let job = |transfer| {
            let action: SshCommand = serde_json::from_value(json!({
                "connection": reference, "remote": transfer,
            }))
            .unwrap();
            Job::new(
                id.clone(),
                "Recurring Windows rsync receiver",
                action.validated().unwrap(),
            )
            .unwrap()
            .with_schedule(JobSchedule::default().with_interval_minutes(NonZeroU32::new(1)))
        };
        let issued = SystemTime::now() - Duration::from_secs(15 * 60);
        let grant = standing::Grant::new(&job(transfer.clone()), &TimeZone::UTC, issued).unwrap();
        transfer["standing"]["approval"] = json!(grant.approval);
        let path = root.join("jobs.json");
        let graph = JobGraph::new(vec![job(transfer)]).unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        standing::Store::new(root.clone())
            .swap(&id, grant, |_| true)
            .unwrap();

        let mut last = initial;
        let mut recoveries: Vec<PathBuf> = Vec::new();
        for firing in 0..3 {
            let displaced_file = root.join("displaced same-content file");
            if firing == 2 {
                fs::rename(&content, &displaced_file).unwrap();
                fs::copy(&displaced_file, &content).unwrap();
            }
            let before = local_existing::inspect(&destination, &|| false).unwrap();
            if firing == 2 {
                assert_eq!(before.inventory, last.inventory);
                assert_ne!(before, last);
            } else {
                assert_eq!(before, last);
            }
            let mut scheduler = SchedulerWindow::open(path.clone()).unwrap();
            scheduler.set_connections(Some(&connections));
            scheduler.set_execution_allowed(true);
            let scheduled = issued + Duration::from_secs(firing * 120);
            scheduler
                .initialize_schedules_at(TimeZone::UTC, scheduled)
                .unwrap();
            let time = scheduled + Duration::from_secs(90);
            let mut jobs = scheduler.update_runtime_at(Message::Tick, time);
            assert_eq!(jobs.len(), 1);
            let request = jobs.pop().unwrap();
            assert_eq!(request.job, id);
            assert_eq!(request.trigger, RunTrigger::Scheduled);
            let result = self.execute_request(&mut scheduler, &request);
            last = local_existing::inspect(&destination, &|| false).unwrap();
            let mut found = recovery_paths(&destination);
            found.sort_unstable();
            if firing == 2 {
                assert_eq!(result.outcome, RunOutcome::Blocked, "{result:?}");
                assert!(
                    result
                        .detail
                        .as_deref()
                        .unwrap()
                        .contains("reviewed local rsync directory changed")
                );
                assert_eq!(last, before);
                assert_eq!(found, recoveries);
            } else {
                assert_eq!(result.outcome, RunOutcome::Success, "{result:?}");
                assert!(!last.aliases(&before));
                assert!(!destination.join("obsolete").exists());
                assert!(destination.join("empty").is_dir());
                assert_eq!(found.len(), usize::try_from(firing + 1).unwrap());
                let newly_recovered = found
                    .iter()
                    .find(|path| !recoveries.contains(path))
                    .unwrap();
                let previous_content: &[u8] = if firing == 0 {
                    b"original Windows receiver"
                } else {
                    SERVER_BYTES
                };
                assert_eq!(
                    fs::read(newly_recovered.join("nested/content")).unwrap(),
                    previous_content
                );
                assert!(
                    result
                        .detail
                        .as_deref()
                        .unwrap()
                        .contains(newly_recovered.to_str().unwrap())
                );
            }
            assert_eq!(fs::read(&content).unwrap(), SERVER_BYTES);
            recoveries = found;
            scheduler.update_runtime_at(Message::Finished(result), time + Duration::from_secs(10));
        }
    }

    fn save_connection(&self, root: &Path) -> (Store, ConnectionRef) {
        let mut connections = Store::open(root.join("connections.json")).unwrap();
        let connection = connections
            .save(Connection {
                id: 0,
                name: "Windows rsync fixture".into(),
                tag: String::new(),
                login: SavedLogin {
                    endpoint: LoginTarget {
                        host: self.endpoint.host().to_owned(),
                        port: self.endpoint.port(),
                        username: self.username.clone(),
                    },
                    auth: Auth::KeyFile {
                        path: self.key_file.clone(),
                        passphrase: None,
                    },
                    keyboard_interactive: false,
                },
                jump: None,
                emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
                tunnels: Vec::new(),
                incognito: false,
                isolation: Default::default(),
                startup: Default::default(),
                logging: Default::default(),
                terminal_effect: None,
                command_timestamps: None,
                tab_color: None,
                paste_warning: None,
                window_geometry: None,
                terminal: Default::default(),
            })
            .unwrap();
        let reference = ConnectionRef::from(connections.get(connection).unwrap());
        (connections, reference)
    }

    fn run_saved(&self, scheduler: &mut SchedulerWindow, view: &mut SchedulerView) -> Completion {
        let request = scheduler
            .update_view(view, Message::Run(false))
            .pop()
            .unwrap();
        let completion = self.execute_request(scheduler, &request);
        scheduler.update_runtime(Message::Finished(completion.clone()));
        completion
    }

    fn execute_request(
        &self,
        scheduler: &mut SchedulerWindow,
        request: &RunRequest<SshCommand>,
    ) -> Completion {
        let mut policy = ExecutionPolicy::unrestricted(true);
        policy.rsync = scheduler.try_rsync_run_context(request).unwrap();
        assert!(policy.rsync.is_some());
        let (transfer, upload) = request
            .action
            .remote_action()
            .unwrap()
            .rsync_transfer()
            .unwrap();
        assert!(!upload);
        let result = run(
            request,
            transfer,
            upload,
            &policy,
            SshTarget {
                endpoint: self.endpoint.clone(),
                username: self.username.clone(),
                authentication: Authentication::private_key(fs::read(&self.key_file).unwrap()),
            },
            Vec::new(),
            &self.trust,
        );
        result.unwrap_or_else(|error| Completion::from_result(request.id, Err(error)))
    }
}

fn saved_scheduler(root: &Path, connections: &Store, action: SshCommand) -> SchedulerWindow {
    let jobs = root.join("jobs.json");
    let id = JobId::new("windows-rsync-existing-receiver").unwrap();
    let job = Job::new(id, "Windows rsync receiver", action.validated().unwrap()).unwrap();
    let graph = JobGraph::new(vec![job]).unwrap();
    SchedulerStore::open(&jobs)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs).unwrap();
    scheduler.set_connections(Some(connections));
    scheduler.set_execution_allowed(true);
    scheduler
}

fn recovery_paths(destination: &Path) -> Vec<PathBuf> {
    fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(kortty_server_archive::rsync_tree::valid_stage)
        })
        .map(|path| path.join("original"))
        .collect()
}
