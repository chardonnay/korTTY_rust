#![cfg(all(feature = "decoder", any(target_os = "linux", target_os = "macos")))]

use std::ffi::OsString;
use std::fs::{self, DirBuilder, FileTimes, OpenOptions};
use std::io::{self, Cursor, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use kortty_server_archive::manifest::{Manifest, Member, MemberKind};
use kortty_server_archive::protocol::{
    CreateFormat, CreateSpec, MAX_REQUEST_BYTES, OutputMetadata, PASSWORD_COMMIT, PASSWORD_READY,
    Reply, Request, Review, creation_password_body, read_reply, write_request,
};
use sevenz_rust2::{ArchiveReader, EncoderMethod, Password};
use sha2::{Digest, Sha256};

const DIRECTORY: &str = "資料";
const FILE: &str = "資料/δ.bin";
const EMPTY_FILE: &str = "資料/empty.bin";
const PAYLOAD: &[u8] = b"checked\0unicode archive bytes\xff";

#[test]
fn stdio_creates_private_archive_after_review_and_preserves_late_collisions() {
    let fixture = Fixture::new();
    let folder = fixture.0.join(DIRECTORY);
    DirBuilder::new().mode(0o700).create(&folder).unwrap();
    let source = fixture.0.join(FILE);
    write_private(&source, PAYLOAD);
    write_private(&fixture.0.join(EMPTY_FILE), b"");
    write_private(&folder.join("omitted.log"), b"not selected");
    for (format, suffix, level) in [
        (CreateFormat::Zip, ".zip", 6),
        (CreateFormat::TarBzip2, ".tar.bz2", 6),
        (CreateFormat::SevenZip, ".7z", 0),
        (CreateFormat::SevenZip, ".7z", 6),
    ] {
        let destination = fixture.0.join(format!("reviewed-{level}{suffix}"));
        let spec = CreateSpec {
            sources: vec![folder.to_str().unwrap().into()],
            destination: destination.to_str().unwrap().into(),
            format,
            level,
            excludes: vec!["*.log".into()],
            output: None,
            report_payload: false,
        };
        let review = || {
            let Reply::CreationReview { review } =
                invoke(&Request::ReviewCreation { spec: spec.clone() })
            else {
                panic!("expected creation review");
            };
            review.validate_for(&spec).unwrap();
            assert_eq!(review.excluded, 1);
            assert_eq!(review.manifest.expanded_bytes, PAYLOAD.len() as u64);
            review
        };
        let apply = |token| {
            invoke(&Request::ApplyCreation {
                spec: spec.clone(),
                token,
            })
        };
        let stale = review();
        fs::write(&source, [b'x'; PAYLOAD.len()]).unwrap();
        assert_failed(apply(stale.token), "changed");
        assert!(!destination.exists());
        fs::write(&source, PAYLOAD).unwrap();
        let approved = review();
        write_private(&destination, b"late destination");
        assert_failed(apply(approved.token.clone()), "already exists");
        assert_eq!(fs::read(&destination).unwrap(), b"late destination");
        fs::remove_file(&destination).unwrap();
        assert_applied(apply(approved.token), &destination);
        assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o600);
        let bytes = fs::read(destination).unwrap();
        let mut found = Vec::new();
        let decoded_format = match format {
            CreateFormat::Zip | CreateFormat::ZipPassword => kortty_server_archive::Format::Zip,
            CreateFormat::Tar => kortty_server_archive::Format::Tar,
            CreateFormat::TarBzip2 => kortty_server_archive::Format::TarBzip2,
            CreateFormat::SevenZip => kortty_server_archive::Format::SevenZip,
        };
        let manifest =
            kortty_server_archive::decode::visit(decoded_format, &bytes, |member, reader| {
                if member.path == FILE {
                    reader.read_to_end(&mut found)?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(manifest.members.len(), 3);
        assert!(manifest.members.iter().any(|member| {
            member.path == EMPTY_FILE && member.kind == MemberKind::File && member.size == 0
        }));
        assert_eq!(found, PAYLOAD);
        if format == CreateFormat::SevenZip {
            let mut archive = ArchiveReader::new(Cursor::new(&bytes), Password::empty()).unwrap();
            let mut methods = Vec::new();
            archive
                .file_compression_methods(FILE, &mut methods)
                .unwrap();
            assert_eq!(
                methods,
                [if level == 0 {
                    EncoderMethod::COPY
                } else {
                    EncoderMethod::LZMA2
                }]
            );
            assert!(archive.read_file(EMPTY_FILE).unwrap().is_empty());
        }
    }
    assert!(
        !names(&fixture.0)
            .iter()
            .any(|name| name.to_string_lossy().starts_with(".kortty-"))
    );
}

#[test]
fn stdio_binds_archive_output_metadata_to_review_and_applies_it_before_publication() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.txt");
    let destination = fixture.0.join("reviewed.zip");
    write_private(&source, PAYLOAD);
    let uid = rustix::process::geteuid().as_raw();
    let gid = rustix::process::getegid().as_raw();
    let spec = |uid, gid, mode| CreateSpec {
        sources: vec![source.to_str().unwrap().into()],
        destination: destination.to_str().unwrap().into(),
        format: CreateFormat::Zip,
        level: 6,
        excludes: Vec::new(),
        output: Some(OutputMetadata { uid, gid, mode }),
        report_payload: false,
    };
    let review = |spec: &CreateSpec| {
        let Reply::CreationReview { review } =
            invoke(&Request::ReviewCreation { spec: spec.clone() })
        else {
            panic!("expected archive output review");
        };
        review.validate_for(spec).unwrap();
        review
    };
    let apply = |spec: CreateSpec, token| invoke(&Request::ApplyCreation { spec, token });

    for invalid in [
        spec(Some(u32::MAX), None, None),
        spec(None, None, Some(0o4755)),
    ] {
        assert_failed(
            invoke(&Request::ReviewCreation { spec: invalid }),
            "ownership IDs",
        );
    }
    let selected = spec(Some(uid), Some(gid), Some(0o644));
    let approved = review(&selected);
    let changed = spec(Some(uid), Some(gid), Some(0o000));
    assert!(approved.validate_for(&changed).is_err());
    assert_failed(apply(changed.clone(), approved.token), "changed");
    assert!(!destination.exists());

    if uid != 0 {
        let unauthorized = spec(Some(0), None, Some(0o644));
        let approved = review(&unauthorized);
        assert!(matches!(
            apply(unauthorized, approved.token),
            Reply::Failed {
                outcome_unknown: false,
                ..
            }
        ));
        assert!(!destination.exists());
    }

    for (spec, expected) in [(selected, 0o644), (changed, 0o000)] {
        let approved = review(&spec);
        assert_applied(apply(spec, approved.token), &destination);
        let actual = fs::metadata(&destination).unwrap();
        assert_eq!(
            (
                actual.uid(),
                actual.gid(),
                actual.mode() & 0o7777,
                actual.nlink()
            ),
            (uid, gid, expected, 1)
        );
        fs::remove_file(&destination).unwrap();
    }
    assert_eq!(names(&fixture.0), [OsString::from("source.txt")]);
}

#[test]
fn stdio_returns_a_bounded_digest_receipt_for_the_exact_published_archive() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.txt");
    let destination = fixture.0.join("reviewed.zip");
    write_private(&source, PAYLOAD);
    let mut spec = CreateSpec {
        sources: vec![source.to_str().unwrap().into()],
        destination: destination.to_str().unwrap().into(),
        format: CreateFormat::Zip,
        level: 6,
        excludes: Vec::new(),
        output: None,
        report_payload: false,
    };
    let review = |spec: &CreateSpec| {
        let Reply::CreationReview { review } =
            invoke(&Request::ReviewCreation { spec: spec.clone() })
        else {
            panic!("expected archive review");
        };
        review.validate_for(spec).unwrap();
        review
    };
    let legacy = review(&spec);
    assert!(
        !serde_json::to_value(&legacy.spec)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("report_payload")
    );
    spec.report_payload = true;
    assert_failed(
        invoke(&Request::ApplyCreation {
            spec: spec.clone(),
            token: legacy.token,
        }),
        "changed",
    );
    assert!(!destination.exists());

    let approved = review(&spec);
    let Reply::CreationApplied {
        destination: actual,
        payload,
    } = invoke(&Request::ApplyCreation {
        spec,
        token: approved.token,
    })
    else {
        panic!("expected publication with payload receipt");
    };
    payload.validate().unwrap();
    assert_eq!(Path::new(&actual), destination);
    let bytes = fs::read(destination).unwrap();
    assert_eq!(payload.bytes, bytes.len() as u64);
    assert_eq!(payload.sha256, <[u8; 32]>::from(Sha256::digest(bytes)));
}

#[test]
fn stdio_password_zip_requires_a_separate_commit_and_encrypts_the_exact_payload() {
    const PASSWORD: &str = "fixture 東京\0\nZIP password";
    let fixture = Fixture::new();
    let source = fixture.0.join("source.txt");
    let destination = fixture.0.join("reviewed.zip");
    write_private(&source, PAYLOAD);
    let spec = CreateSpec {
        sources: vec![source.to_str().unwrap().into()],
        destination: destination.to_str().unwrap().into(),
        format: CreateFormat::ZipPassword,
        level: 6,
        excludes: Vec::new(),
        output: None,
        report_payload: true,
    };
    let Reply::CreationReview { review } = invoke(&Request::ReviewCreation { spec: spec.clone() })
    else {
        panic!("expected password ZIP review");
    };
    review.validate_for(&spec).unwrap();
    let mut request = Vec::new();
    write_request(
        &mut request,
        &Request::ApplyCreation {
            spec,
            token: review.token,
        },
    )
    .unwrap();
    let body = creation_password_body(PASSWORD).unwrap();
    assert!(
        !request
            .windows(PASSWORD.len())
            .any(|window| window == PASSWORD.as_bytes())
    );
    for suffix in [
        Vec::new(),
        (1025_u32).to_be_bytes().to_vec(),
        body.to_vec(),
        [body.as_slice(), b"not a commit marker here!"].concat(),
        [body.as_slice(), PASSWORD_COMMIT, b"trailing input"].concat(),
    ] {
        let output = run(&[request.as_slice(), &suffix].concat());
        assert!(!output.status.success());
        assert_eq!(output.stdout, PASSWORD_READY);
        assert!(
            !output
                .stderr
                .windows(PASSWORD.len())
                .any(|window| window == PASSWORD.as_bytes())
        );
        assert!(!destination.exists());
    }
    let wire = [request.as_slice(), body.as_slice(), PASSWORD_COMMIT].concat();
    write_private(&destination, b"late destination");
    let late = run(&wire);
    assert!(late.status.success());
    assert_failed(
        read_reply(late.stdout.strip_prefix(PASSWORD_READY).unwrap()).unwrap(),
        "already exists",
    );
    assert_eq!(fs::read(&destination).unwrap(), b"late destination");
    fs::remove_file(&destination).unwrap();
    let output = run(&wire);
    assert!(output.status.success() && output.stderr.is_empty());
    let Reply::CreationApplied {
        destination: actual,
        payload,
    } = read_reply(output.stdout.strip_prefix(PASSWORD_READY).unwrap()).unwrap()
    else {
        panic!("expected password ZIP payload receipt");
    };
    assert_eq!(Path::new(&actual), destination);
    let bytes = fs::read(destination).unwrap();
    assert_eq!(payload.bytes, bytes.len() as u64);
    assert_eq!(payload.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert!(zip.by_name("source.txt").is_err());
    assert!(
        zip.by_name_decrypt("source.txt", b"wrong password")
            .is_err()
    );
    let mut decoded = Vec::new();
    zip.by_name_decrypt("source.txt", PASSWORD.as_bytes())
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
    assert_eq!(decoded, PAYLOAD);
}

#[test]
fn stdio_reviews_a_unicode_tar_and_publishes_the_exact_numbered_sibling() {
    let fixture = Fixture::new();
    let source = fixture.0.join("archive-雪.tar");
    let original = tar_bytes();
    write_private(&source, &original);
    let occupied = fixture.0.join("archive-雪");
    DirBuilder::new().mode(0o700).create(&occupied).unwrap();
    write_private(&occupied.join("keep"), b"existing sibling");
    let before = names(&fixture.0);

    let review = review(&source);
    let destination = fixture.0.join("archive-雪 (1)");
    assert_eq!(Path::new(&review.source), source);
    assert_eq!(Path::new(&review.destination), destination);
    assert_eq!(
        review.manifest,
        Manifest {
            members: vec![
                member(
                    "copy",
                    MemberKind::Hardlink {
                        target: FILE.into()
                    },
                    0
                ),
                member("empty", MemberKind::Directory, 0),
                member(DIRECTORY, MemberKind::Directory, 0),
                member(
                    "資料/alias",
                    MemberKind::Symlink {
                        target: FILE.into()
                    },
                    0
                ),
                member(FILE, MemberKind::File, PAYLOAD.len() as u64),
            ],
            expanded_bytes: PAYLOAD.len() as u64,
        }
    );
    assert_eq!(names(&fixture.0), before);
    assert_eq!(fs::read(&source).unwrap(), original);

    assert_applied(invoke(&approved(&review)), &destination);
    assert_eq!(fs::read(destination.join(FILE)).unwrap(), PAYLOAD);
    assert!(destination.join("empty").is_dir());
    assert!(destination.join("資料/alias").is_symlink());
    assert_eq!(fs::read(destination.join("資料/alias")).unwrap(), PAYLOAD);
    let copy = fs::metadata(destination.join("copy")).unwrap();
    let original_entry = fs::metadata(destination.join(FILE)).unwrap();
    assert_eq!(
        (copy.dev(), copy.ino()),
        (original_entry.dev(), original_entry.ino())
    );
    assert_eq!(original_entry.mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::read(occupied.join("keep")).unwrap(),
        b"existing sibling"
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        names(&fixture.0),
        ["archive-雪", "archive-雪 (1)", "archive-雪.tar"].map(OsString::from),
    );
}

#[test]
fn stdio_refuses_a_late_collision_and_requires_review_after_source_bytes_change() {
    let fixture = Fixture::new();
    let source = fixture.0.join("sample.zip");
    let original = zip_bytes(b"first bytes");
    let updated = zip_bytes(b"other bytes");
    assert_eq!(original.len(), updated.len());
    write_private(&source, &original);

    let initial = review(&source);
    let collision = fixture.0.join("sample");
    assert_eq!(Path::new(&initial.destination), collision);
    assert_eq!(names(&fixture.0), [OsString::from("sample.zip")]);
    write_private(&collision, b"concurrent sibling");
    let before = names(&fixture.0);
    assert_failed(invoke(&approved(&initial)), "already exists");
    assert_eq!(
        names(&fixture.0),
        before,
        "no stage or alternate destination remains"
    );
    assert_eq!(fs::read(&collision).unwrap(), b"concurrent sibling");
    assert_eq!(fs::read(&source).unwrap(), original);

    let stale = review(&source);
    let destination = fixture.0.join("sample (1)");
    assert_eq!(Path::new(&stale.destination), destination);
    let modified = fs::metadata(&source).unwrap().modified().unwrap();
    let mut archive = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&source)
        .unwrap();
    archive.write_all(&updated).unwrap();
    archive
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    drop(archive);
    assert_failed(invoke(&approved(&stale)), "changed");
    assert_eq!(names(&fixture.0), before, "a stale review creates no files");

    let current = review(&source);
    assert_eq!(current.manifest, stale.manifest);
    assert_ne!(current.token, stale.token);
    assert_applied(invoke(&approved(&current)), &destination);
    assert_eq!(
        fs::read(destination.join("payload.txt")).unwrap(),
        b"other bytes"
    );
    assert_eq!(fs::read(&collision).unwrap(), b"concurrent sibling");
    assert_eq!(fs::read(&source).unwrap(), updated);
    assert_eq!(
        names(&fixture.0),
        ["sample", "sample (1)", "sample.zip"].map(OsString::from)
    );
}

#[test]
fn stdio_rejects_incomplete_oversized_and_trailing_request_frames_before_writing() {
    let fixture = Fixture::new();
    let source = fixture.0.join("sample.zip");
    let original = zip_bytes(b"payload");
    write_private(&source, &original);
    let review = review(&source);
    let mut valid = Vec::new();
    write_request(&mut valid, &approved(&review)).unwrap();
    let incomplete = valid[..valid.len() - 1].to_vec();
    let oversized = (MAX_REQUEST_BYTES as u32 + 1).to_be_bytes().to_vec();
    let mut trailing = valid;
    trailing.push(b'\n');
    for request in [incomplete, oversized, trailing] {
        let output = run(&request);
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "a rejected frame returns no reply"
        );
        assert_eq!(names(&fixture.0), [OsString::from("sample.zip")]);
        assert_eq!(fs::read(&source).unwrap(), original);
    }
}

fn member(path: &str, kind: MemberKind, size: u64) -> Member {
    Member {
        path: path.into(),
        kind,
        size,
    }
}

fn tar_bytes() -> Vec<u8> {
    let mut writer = tar::Builder::new(Vec::new());
    for (name, target, kind) in [
        ("copy", "./資料/δ.bin", tar::EntryType::Link),
        ("資料/alias", "./δ.bin", tar::EntryType::Symlink),
    ] {
        let mut header = tar_header(kind, 0);
        writer.append_link(&mut header, name, target).unwrap();
    }
    writer
        .append_data(
            &mut tar_header(tar::EntryType::Directory, 0),
            "empty",
            io::empty(),
        )
        .unwrap();
    writer
        .append_data(
            &mut tar_header(tar::EntryType::Regular, PAYLOAD.len() as u64),
            FILE,
            PAYLOAD,
        )
        .unwrap();
    writer.into_inner().unwrap()
}

fn tar_header(kind: tar::EntryType, size: u64) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(kind);
    header.set_mode(0o755);
    header.set_size(size);
    header
}

fn zip_bytes(payload: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("payload.txt", options).unwrap();
    writer.write_all(payload).unwrap();
    writer.finish().unwrap().into_inner()
}

fn review(source: &Path) -> Review {
    let reply = invoke(&Request::Review {
        source: source.to_str().unwrap().into(),
    });
    let Reply::Review { review } = reply else {
        panic!("expected archive review, received {reply:?}");
    };
    review
}

fn approved(review: &Review) -> Request {
    Request::Apply {
        source: review.source.clone(),
        destination: review.destination.clone(),
        token: review.token.clone(),
    }
}

fn assert_applied(reply: Reply, expected: &Path) {
    let Reply::Applied { destination } = reply else {
        panic!("expected publication, received {reply:?}");
    };
    assert_eq!(Path::new(&destination), expected);
}

fn assert_failed(reply: Reply, expected_message: &str) {
    let Reply::Failed {
        message,
        outcome_unknown,
    } = reply
    else {
        panic!("expected refusal, received {reply:?}");
    };
    assert!(message.contains(expected_message), "{message}");
    assert!(!outcome_unknown, "{message}");
}

fn invoke(request: &Request) -> Reply {
    let mut frame = Vec::new();
    write_request(&mut frame, request).unwrap();
    let output = run(&frame);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    read_reply(output.stdout.as_slice()).unwrap()
}

fn run(frame: &[u8]) -> Output {
    let mut helper = Command::new(env!("CARGO_BIN_EXE_kortty-server-archive"))
        .arg("--stdio-v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let written = helper.stdin.take().unwrap().write_all(frame);
    let output = helper.wait_with_output().unwrap();
    written.unwrap();
    output
}

fn write_private(path: &Path, data: &[u8]) {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(data)
        .unwrap();
}

fn names(path: &Path) -> Vec<OsString> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort_unstable();
    entries
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().canonicalize().unwrap();
        for _ in 0..256 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("kortty-archive-{}-{sequence}", std::process::id()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("could not create private archive fixture: {error}"),
            }
        }
        panic!("could not reserve a private archive fixture directory");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
