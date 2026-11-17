#![cfg(all(feature = "decoder", target_os = "linux"))]

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use kortty_server_archive::protocol::{
    ArchiveMemberLayout, CreateFormat, CreateReview, CreateSpec, CreatedPayload, UPDATE_ARGUMENT,
    UpdateDecision, UpdateReply, UpdateRequest, creation_password_body, read_update_reply,
    write_update_decision, write_update_request,
};
use kortty_server_archive::rsync_tree::valid_stage;
use sha2::{Digest, Sha256};

#[test]
fn archive_update_recreates_tar_and_returns_the_exact_successor_and_recovery() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.0.join("source");
    DirBuilder::new().mode(0o700).create(&source).unwrap();
    write(&source.join("first"), b"old tar member");
    let destination = fixture.0.join("snapshot.tar");
    let spec = spec(&source, &destination, CreateFormat::Tar);

    let (initial_review, existing, receiver) = review(&spec);
    assert_eq!(existing, None);
    let (mut first, payload) = prepare(&spec, initial_review, None);
    assert!(!destination.exists());
    first.decision(UpdateDecision::Publish);
    let published = assert_published(first.finish(), &destination, payload, None);
    assert_ne!(published, receiver);
    assert_eq!(file_payload(&destination), payload);
    assert_eq!(tar_files(&destination), ["source/first"]);
    assert_no_private_stage(&fixture.0);

    fs::remove_file(source.join("first")).unwrap();
    write(&source.join("second"), b"new tar member");
    let (second_review, existing, receiver) = review(&spec);
    assert_eq!(receiver, published);
    assert_eq!(existing, Some(payload));
    let original_id = fs::metadata(&destination).unwrap().ino();
    let (mut second, next) = prepare(&spec, second_review, None);
    assert_eq!(file_payload(&destination), payload);
    let recovery = second.quarantine();
    assert!(!destination.exists());
    assert_eq!(fs::metadata(&recovery).unwrap().ino(), original_id);
    assert_eq!(file_payload(&recovery), payload);
    second.decision(UpdateDecision::Publish);
    let successor = assert_published(second.finish(), &destination, next, Some(&recovery));
    assert_ne!(successor, published);
    assert_eq!(tar_files(&destination), ["source/second"]);
    assert_eq!(tar_files(&recovery), ["source/first"]);
    let (_, existing, receiver) = review(&spec);
    assert_eq!(existing, Some(next));
    assert_eq!(receiver, successor);
}

#[test]
fn archive_update_recreates_java_tar_paths_and_prunes_excluded_subtrees() {
    use std::collections::BTreeSet;
    use std::io::Read;

    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let selected = fixture.0.join("selected");
    let ignored = selected.join("ignored");
    let retained = selected.join("keepdir");
    let external = fixture.0.join("other/ignored");
    for path in [&ignored, &retained, &external] {
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path)
            .unwrap();
    }
    let excluded_source = external.join("explicit.txt");
    write(&ignored.join("file"), b"excluded subtree");
    write(&excluded_source, b"excluded by an ancestor");
    write(
        &retained.join("file"),
        b"kept because the pattern ends with a slash",
    );
    let stored = |path: &Path| path.strip_prefix("/").unwrap().to_str().unwrap().to_owned();
    let expected = BTreeSet::from([
        (stored(&selected), true),
        (stored(&retained), true),
        (stored(&retained.join("file")), false),
    ]);

    for (format, suffix) in [
        (CreateFormat::Tar, "tar"),
        (CreateFormat::TarBzip2, "tar.bz2"),
    ] {
        let destination = fixture.0.join(format!("snapshot.{suffix}"));
        write(&destination, b"prior archive bytes");
        let mut spec = spec(&selected, &destination, format);
        spec.member_layout = ArchiveMemberLayout::ServerPath;
        spec.sources.push(excluded_source.to_str().unwrap().into());
        spec.excludes = vec!["ignored".into(), "keepdir/".into()];
        let original = file_payload(&destination);
        let (review, existing, _) = review(&spec);
        assert_eq!(review.excluded, 2);
        assert_eq!(existing, Some(original));
        let (mut helper, payload) = prepare(&spec, review, None);
        let recovery = helper.quarantine();
        helper.decision(UpdateDecision::Publish);
        assert_published(helper.finish(), &destination, payload, Some(&recovery));
        assert_eq!(file_payload(&recovery), original);

        let file = File::open(destination).unwrap();
        let input: Box<dyn Read> = if format == CreateFormat::TarBzip2 {
            Box::new(bzip2::read::BzDecoder::new(file))
        } else {
            Box::new(file)
        };
        let actual = tar::Archive::new(input)
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry
                        .path()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .trim_end_matches('/')
                        .to_owned(),
                    entry.header().entry_type().is_dir(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }
}

#[test]
fn archive_update_rejects_an_identical_substitute_and_restores_after_disconnect() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.0.join("source");
    write(&source, b"incoming bytes");
    let destination = fixture.0.join("snapshot.tar");
    let spec = spec(&source, &destination, CreateFormat::Tar);
    write(&destination, b"receiver used for TAR regeneration");
    fs::set_permissions(&destination, Permissions::from_mode(0o640)).unwrap();
    let (stale, _, prior_fingerprint) = review(&spec);
    let moved = fixture.0.join("moved.tar");
    fs::rename(&destination, &moved).unwrap();
    write(&destination, &fs::read(&moved).unwrap());
    fs::set_permissions(&destination, Permissions::from_mode(0o640)).unwrap();
    let helper = Helper::prepare(&spec, stale.token);
    assert!(matches!(
        helper.finish(),
        UpdateReply::Failed {
            outcome_unknown: false,
            recovery: None,
            ..
        }
    ));
    assert_no_private_stage(&fixture.0);
    let (current, original, current_fingerprint) = review(&spec);
    assert_ne!(current_fingerprint, prior_fingerprint);
    let original_id = fs::metadata(&destination).unwrap().ino();

    let (mut interrupted, _) = prepare(&spec, current, None);
    let recovery = interrupted.quarantine();
    assert!(!destination.exists());
    assert_eq!(fs::metadata(&recovery).unwrap().ino(), original_id);
    assert!(matches!(interrupted.finish(), UpdateReply::Aborted));
    assert_eq!(fs::metadata(&destination).unwrap().ino(), original_id);
    assert_eq!(Some(file_payload(&destination)), original);
    assert_no_private_stage(&fixture.0);

    let (current, _, _) = review(&spec);
    let (mut publish, next) = prepare(&spec, current, None);
    let recovery = publish.quarantine();
    publish.decision(UpdateDecision::Publish);
    assert_published(publish.finish(), &destination, next, Some(&recovery));
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o640);
    assert_eq!(fs::metadata(&recovery).unwrap().ino(), original_id);
}

#[test]
fn archive_update_keeps_the_late_occupant_and_the_displaced_original() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.0.join("source");
    write(&source, b"incoming bytes");
    let destination = fixture.0.join("snapshot.tar");
    write(&destination, b"the original");
    let spec = spec(&source, &destination, CreateFormat::Tar);
    let (review, _, _) = review(&spec);
    let (mut helper, _) = prepare(&spec, review, None);
    let recovery = helper.quarantine();
    write(&destination, b"a late occupant");
    helper.decision(UpdateDecision::Publish);
    assert!(
        matches!(helper.finish(), UpdateReply::Failed { outcome_unknown: true, recovery: Some(actual), .. } if Path::new(&actual) == recovery)
    );
    assert_eq!(fs::read(&destination).unwrap(), b"a late occupant");
    assert_eq!(fs::read(&recovery).unwrap(), b"the original");
}

#[test]
fn archive_update_password_handshake_waits_for_explicit_publication() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.0.join("source");
    write(&source, b"one encrypted member");
    let destination = fixture.0.join("snapshot.zip");
    let spec = spec(&source, &destination, CreateFormat::ZipPassword);
    let (review, _, _) = review(&spec);
    let password = "a test-only archive password";
    let (mut helper, payload) = prepare(&spec, review, Some(password));
    assert!(!destination.exists());
    helper.decision(UpdateDecision::Publish);
    assert_published(helper.finish(), &destination, payload, None);
    assert_eq!(file_payload(&destination), payload);
    let archive = zip::ZipArchive::new(File::open(destination).unwrap()).unwrap();
    assert!(archive.by_index_data(0).unwrap().encrypted());
    assert_no_private_stage(&fixture.0);
}

#[test]
fn archive_update_with_server_paths_replaces_java_zip_members_and_keeps_unmatched_entries() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let left = fixture.0.join("left");
    let right = fixture.0.join("right");
    let selected_directory = right.join("files");
    for path in [&left, &right, &selected_directory] {
        DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    let left_file = left.join("report.txt");
    let right_file = right.join("report.txt");
    let kept = selected_directory.join("kept.txt");
    let skipped = selected_directory.join("skipped.txt");
    write(&left_file, b"updated Java ZIP member");
    write(&right_file, b"a disjoint source with the same basename");
    write(&kept, b"inside a directory excluded by its member name");
    write(&skipped, b"absolute exclusion");
    let stored = |path: &Path| path.strip_prefix("/").unwrap().to_str().unwrap().to_owned();
    let destination = fixture.0.join("snapshot.zip");
    let old = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)
        .unwrap();
    let mut zip = zip::ZipWriter::new(old);
    zip.start_file(stored(&left_file), zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"earlier member using its absolute path without the initial slash")
        .unwrap();
    zip.start_file("unmatched.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"retained Java ZIP member").unwrap();
    zip.finish().unwrap();
    let original = file_payload(&destination);

    let mut spec = spec(&left_file, &destination, CreateFormat::Zip);
    spec.member_layout = ArchiveMemberLayout::ServerPath;
    spec.sources.push(right_file.to_str().unwrap().into());
    spec.sources
        .push(selected_directory.to_str().unwrap().into());
    spec.excludes = vec![
        format!("{}/", stored(&selected_directory)),
        skipped.to_str().unwrap().into(),
        format!("./{}", kept.display()),
    ];
    let (review, existing, _) = review(&spec);
    assert_eq!(existing, Some(original));
    assert_eq!(review.excluded, 2);
    for path in [&fixture.0, &selected_directory] {
        assert!(review.manifest.members.iter().any(|member| {
            member.path == stored(path)
                && member.kind == kortty_server_archive::manifest::MemberKind::Directory
        }));
    }
    let (mut helper, payload) = prepare(&spec, review, None);
    let recovery = helper.quarantine();
    helper.decision(UpdateDecision::Publish);
    assert_published(helper.finish(), &destination, payload, Some(&recovery));
    assert_eq!(file_payload(&recovery), original);
    let mut zip = zip::ZipArchive::new(File::open(&destination).unwrap()).unwrap();
    assert_eq!(zip.len(), 4);
    for (path, expected) in [
        (stored(&left_file), &b"updated Java ZIP member"[..]),
        (
            stored(&right_file),
            &b"a disjoint source with the same basename"[..],
        ),
        (
            stored(&kept),
            &b"inside a directory excluded by its member name"[..],
        ),
        ("unmatched.txt".into(), &b"retained Java ZIP member"[..]),
    ] {
        let mut bytes = Vec::new();
        zip.by_name(&path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
}

fn spec(source: &Path, destination: &Path, format: CreateFormat) -> CreateSpec {
    CreateSpec {
        sources: vec![source.to_str().unwrap().into()],
        destination: destination.to_str().unwrap().into(),
        format,
        level: 1,
        excludes: Vec::new(),
        member_layout: Default::default(),
        output: None,
        report_payload: false,
    }
}

fn review(spec: &CreateSpec) -> (CreateReview, Option<CreatedPayload>, [u8; 32]) {
    let mut helper = Helper::start();
    write_update_request(
        helper.input.as_mut().unwrap(),
        &UpdateRequest::Review { spec: spec.clone() },
    )
    .unwrap();
    match helper.finish() {
        UpdateReply::Reviewed {
            review,
            existing,
            receiver,
        } => {
            review.validate_for(spec).unwrap();
            assert_ne!(receiver, [0; 32]);
            (review, existing, receiver)
        }
        reply => panic!("expected a checked archive review, got {reply:?}"),
    }
}

fn prepare(
    spec: &CreateSpec,
    review: CreateReview,
    password: Option<&str>,
) -> (Helper, CreatedPayload) {
    let mut helper = Helper::prepare(spec, review.token);
    if let Some(password) = password {
        assert!(matches!(helper.receive(), UpdateReply::PasswordRequired));
        let input = helper.input.as_mut().unwrap();
        input
            .write_all(&creation_password_body(password).unwrap())
            .unwrap();
        input.flush().unwrap();
    }
    match helper.receive() {
        UpdateReply::Prepared { payload } => (helper, payload),
        reply => panic!("expected a prepared archive, got {reply:?}"),
    }
}

fn assert_published(
    reply: UpdateReply,
    target: &Path,
    expected: CreatedPayload,
    recovery: Option<&Path>,
) -> [u8; 32] {
    match reply {
        UpdateReply::Published {
            destination,
            payload,
            recovery: actual,
            receiver,
        } => {
            assert_eq!(Path::new(&destination), target);
            assert_eq!(payload, expected);
            assert_eq!(actual.as_deref().map(Path::new), recovery);
            assert_ne!(receiver, [0; 32]);
            receiver
        }
        reply => panic!("expected an archive publication receipt, got {reply:?}"),
    }
}

fn file_payload(path: &Path) -> CreatedPayload {
    let bytes = fs::read(path).unwrap();
    CreatedPayload {
        bytes: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
    }
}

fn tar_files(path: &Path) -> Vec<String> {
    tar::Archive::new(File::open(path).unwrap())
        .entries()
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.header().entry_type().is_file())
        .map(|entry| entry.path().unwrap().to_str().unwrap().to_owned())
        .collect()
}

fn write(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn assert_no_private_stage(path: &Path) {
    assert!(
        fs::read_dir(path)
            .unwrap()
            .all(|entry| !valid_stage(entry.unwrap().file_name().to_str().unwrap()))
    );
}

struct Helper {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Helper {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kortty-server-archive"))
            .arg(UPDATE_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        Self {
            child,
            input,
            output,
        }
    }

    fn prepare(spec: &CreateSpec, token: String) -> Self {
        let mut helper = Self::start();
        write_update_request(
            helper.input.as_mut().unwrap(),
            &UpdateRequest::Prepare {
                spec: spec.clone(),
                token,
            },
        )
        .unwrap();
        helper
    }

    fn receive(&mut self) -> UpdateReply {
        read_update_reply(&mut self.output).unwrap()
    }

    fn decision(&mut self, decision: UpdateDecision) {
        write_update_decision(self.input.as_mut().unwrap(), &decision).unwrap();
    }

    fn quarantine(&mut self) -> PathBuf {
        self.decision(UpdateDecision::Quarantine);
        match self.receive() {
            UpdateReply::Quarantined { recovery } => {
                let path = PathBuf::from(recovery);
                assert_eq!(path.file_name().unwrap(), "original");
                assert!(valid_stage(
                    path.parent()
                        .unwrap()
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                ));
                path
            }
            reply => panic!("expected checked archive recovery, got {reply:?}"),
        }
    }

    fn finish(mut self) -> UpdateReply {
        drop(self.input.take());
        let reply = self.receive();
        assert!(self.child.wait().unwrap().success());
        reply
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-archive-update-{}", hex::encode(nonce)));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
