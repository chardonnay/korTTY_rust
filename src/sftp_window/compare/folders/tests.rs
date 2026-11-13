use std::fs::{self, File};
use std::io::Cursor;
use std::path::Path;
use std::time::UNIX_EPOCH;

use kortty_ssh::sftp::Attributes;

use crate::sftp_window::Side;

use super::*;

const STAGE_ID: &str = "00112233445566778899aabbccddeeff";

#[test]
fn reviewed_sync_matches_private_stage_components_with_native_separators() {
    for path in [
        format!(".kortty-{STAGE_ID}.kortty-part"),
        format!("/folder/.kortty-extract.{STAGE_ID}/payload"),
        format!("/folder/.KORTTY-{STAGE_ID}.KORTTY-PART/payload"),
    ] {
        assert!(contains_private_stage_component(path.as_bytes()), "{path}");
    }
    assert!(!contains_private_stage_component(b"folder/.kortty-project"));
    assert!(!contains_private_stage_component(
        b"folder/.kortty-123.kortty-part"
    ));
    let backslash = format!("folder\\.kortty-{STAGE_ID}.kortty-part\\payload");
    assert!(!contains_private_stage_component(backslash.as_bytes()));
    assert_eq!(
        contains_private_stage(&Location::Local(backslash.into())),
        cfg!(windows)
    );
}

#[test]
fn reviewed_sync_protects_recoverable_stages_as_candidates_and_roots_in_both_directions() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let local = root.join("local");
    let remote_fixture = root.join("remote");
    let local_stage = format!(".kortty-{STAGE_ID}.kortty-part");
    let remote_stage = format!(".kortty-extract.{STAGE_ID}");
    put(&local, "local-only", b"copy", 100);
    put(&remote_fixture, "remote-only", b"copy", 100);
    put(&local, &format!("{local_stage}/entry"), b"preserve", 100);
    put(
        &remote_fixture,
        &format!("{remote_stage}/entry"),
        b"preserve",
        100,
    );
    let request = Request::new(
        [
            Location::Local(local.clone()),
            Location::Local(remote_fixture),
        ],
        "",
    )
    .unwrap();
    let mut review = read(&mut client(), request, || false).unwrap();
    // Candidate selection needs the side and comparison rows only. Reuse the
    // native fixture rows as the server side without opening a connection.
    review.manifests[1].root = Location::Remote(b"/remote".to_vec());
    for (to, source, destination) in [
        (Side::Local, "remote-only", "local-only"),
        (Side::Remote, "local-only", "remote-only"),
    ] {
        assert_eq!(
            review
                .source_only(to)
                .map(Relative::display)
                .collect::<Vec<_>>(),
            [source]
        );
        assert_eq!(
            review
                .destination_only(to)
                .map(Relative::display)
                .collect::<Vec<_>>(),
            [destination]
        );
    }

    review.manifests[0].root = Location::Local(local.join(&local_stage).join("nested"));
    assert_protected_roots(&review);
    review.manifests[0].root = Location::Local(local);
    review.manifests[1].root =
        Location::Remote(format!("/remote/{remote_stage}/nested").into_bytes());
    assert_protected_roots(&review);
}

fn assert_protected_roots(review: &Review) {
    for to in [Side::Local, Side::Remote] {
        assert!(review.direction(to).is_none());
        assert!(review.source_only(to).next().is_none());
        assert!(review.destination_only(to).next().is_none());
        assert!(review.replacement_candidates(to).next().is_none());
    }
}

fn put(root: &Path, path: &str, contents: &[u8], modified: u64) {
    put_at(root, path, contents, Duration::from_secs(modified));
}

fn put_at(root: &Path, path: &str, contents: &[u8], modified: Duration) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + modified)
        .unwrap();
}

fn client() -> Sftp<Cursor<[u8; 9]>, io::Sink> {
    Sftp::new(Cursor::new([0, 0, 0, 5, 2, 0, 0, 0, 3]), io::sink()).unwrap()
}

#[test]
fn reviewed_replacements_match_java_metadata_in_both_directions_and_protect_private_paths() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let local = root.join("local");
    let server = root.join("server");
    let stage = format!(".KORTTY-{STAGE_ID}.KORTTY-PART");
    let stage_member = format!("{stage}/payload");
    for (path, local_bytes, local_millis, remote_bytes, remote_seconds) in [
        (
            "equal-size-newer",
            b"local".as_slice(),
            100_000,
            b"fresh".as_slice(),
            102,
        ),
        ("older-at-boundary", b"longer", 102_000, b"old", 100),
        ("same-size-inside", b"left", 100_001, b"same", 102),
        ("same-time-different-size", b"longer", 100_000, b"x", 100),
        ("size-just-inside", b"longer", 101_999, b"x", 100),
        ("excluded.log", b"local", 100_000, b"fresh", 102),
        (stage_member.as_str(), b"local", 100_000, b"fresh", 102),
    ] {
        put_at(
            &local,
            path,
            local_bytes,
            Duration::from_millis(local_millis),
        );
        put(&server, path, remote_bytes, remote_seconds);
    }
    put(&local, "conflict", b"local file", 100);
    put(&server, "conflict/child", b"remote folder", 102);
    put(&local, "destination-only", b"extra", 100);
    put(&server, "source-only", b"extra", 102);

    let request = Request::new(
        [Location::Local(local.clone()), Location::Local(server)],
        "*.log",
    )
    .unwrap();
    let mut review = read(&mut client(), request, || false).unwrap();
    assert!(review.replacement_candidates(Side::Local).next().is_none());

    // Reuse the local walk's paths with SFTP v3 file attributes for the server side.
    let server = &mut review.manifests[1];
    server.root = Location::Remote(b"/server".to_vec());
    for entry in server.entries.values_mut() {
        *entry = match entry {
            Entry::LocalFile { stamp, .. } => Entry::Remote(Attributes {
                size: Some(stamp.size),
                modified: Some(
                    stamp
                        .modified
                        .unwrap()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as u32,
                ),
                permissions: Some(0o100600),
                ..Attributes::default()
            }),
            Entry::LocalFolder(_) => Entry::Remote(Attributes {
                permissions: Some(0o40700),
                ..Attributes::default()
            }),
            _ => panic!("expected an ordinary fixture entry"),
        };
    }
    for _ in 0..2 {
        review.rows = classify(&review.manifests[0].entries, &review.manifests[1].entries);
        assert_eq!(
            review
                .replacement_candidates(Side::Local)
                .map(Relative::display)
                .collect::<Vec<_>>(),
            [
                "equal-size-newer",
                "same-time-different-size",
                "size-just-inside"
            ]
        );
        assert_eq!(
            review
                .replacement_candidates(Side::Remote)
                .map(Relative::display)
                .collect::<Vec<_>>(),
            [
                "older-at-boundary",
                "same-time-different-size",
                "size-just-inside"
            ]
        );
        review.manifests.swap(0, 1);
    }
    review.manifests[0].root = Location::Local(local.join(stage));
    assert_protected_roots(&review);
}

#[test]
fn sftp_folder_comparison_reports_metadata_and_prunes_reviewed_excludes() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let left = root.join("left");
    let right = root.join("right");
    put(&left, "docs/same", b"one", 100);
    put(&right, "docs/same", b"two", 101);
    put(&left, "docs/newer", b"new", 102);
    put(&right, "docs/newer", b"old", 100);
    put(&left, "docs/size", b"long", 100);
    put(&right, "docs/size", b"x", 100);
    put(&left, "conflict/child", b"hidden by parent", 100);
    put(&right, "conflict", b"file", 100);
    put(&left, "ignore.log", b"skip", 100);
    put(&right, "docs/ignore.log", b"skip", 100);
    put(&left, "docs/ignored/secret", b"skip", 100);
    put(&right, "docs/ignored/secret", b"skip", 100);
    put(
        &right,
        "other/ignored/keep",
        b"outside root-relative glob",
        100,
    );
    let request = Request {
        roots: [Location::Local(left), Location::Local(right)],
        excludes: excludes("*.log\ndocs/ignored").unwrap(),
    };
    let review = read(&mut client(), request, || false).unwrap();
    let states: BTreeMap<_, _> = review
        .rows
        .iter()
        .map(|(path, state)| (path.display(), *state))
        .collect();
    assert_eq!(states["docs"], State::ContentsDiffer);
    assert_eq!(states["docs/same"], State::Same);
    assert_eq!(states["docs/newer"], State::LeftNewer);
    assert_eq!(states["docs/size"], State::DifferentSize);
    assert_eq!(states["conflict"], State::TypeConflict);
    assert_eq!(states["conflict/child"], State::BlockedByParent);
    assert_eq!(states["other/ignored/keep"], State::RightOnly);
    assert!(
        !states
            .keys()
            .any(|path| path.ends_with(".log") || path.starts_with("docs/ignored"))
    );
}

#[cfg(unix)]
#[test]
fn sftp_folder_comparison_keeps_link_and_raw_name_identities() {
    use std::ffi::OsStr;
    use std::os::unix::fs::symlink;

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let left = root.join("left");
    let right = root.join("right");
    put(&right, "link/child", b"blocked", 100);
    fs::create_dir(&left).unwrap();
    symlink(&right, left.join("link")).unwrap();
    // APFS requires UTF-8 names. Retain invalid-byte coverage on other Unix hosts.
    #[cfg(target_os = "macos")]
    let names = [
        (OsStr::new("raw-猫"), "raw-猫"),
        (OsStr::new("raw-犬"), "raw-犬"),
    ];
    #[cfg(not(target_os = "macos"))]
    let names = {
        use std::os::unix::ffi::OsStrExt as _;

        [
            (OsStr::from_bytes(b"raw\xff"), r"raw\xff"),
            (OsStr::from_bytes(b"raw\xfe"), r"raw\xfe"),
        ]
    };
    fs::write(left.join(names[0].0), b"one").unwrap();
    fs::write(right.join(names[1].0), b"two").unwrap();
    let request = Request {
        roots: [Location::Local(left), Location::Local(right)],
        excludes: excludes("").unwrap(),
    };
    let review = read(&mut client(), request, || false).unwrap();
    let states: BTreeMap<_, _> = review
        .rows
        .iter()
        .map(|(path, state)| (path.display(), *state))
        .collect();
    assert_eq!(states["link"], State::Blocked);
    assert_eq!(states["link/child"], State::BlockedByParent);
    assert_eq!(states[names[0].1], State::LeftOnly);
    assert_eq!(states[names[1].1], State::RightOnly);
}

#[test]
fn sftp_folder_comparison_rejects_invalid_excludes_and_deep_or_cancelled_scans() {
    assert!(excludes("../outside").is_err());
    assert!(excludes("[unfinished").is_err());
    assert!(excludes(&"pattern\n".repeat(MAX_PATTERNS + 1)).is_err());
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let left = root.join("left");
    let right = root.join("right");
    fs::create_dir_all(left.join("a/".repeat(33))).unwrap();
    fs::create_dir(&right).unwrap();
    let request = || Request {
        roots: [
            Location::Local(left.clone()),
            Location::Local(right.clone()),
        ],
        excludes: excludes("").unwrap(),
    };
    assert!(
        read(&mut client(), request(), || false)
            .unwrap_err()
            .message
            .contains("32 nested levels")
    );
    assert!(
        read(&mut client(), request(), || true)
            .unwrap_err()
            .message
            .contains("cancelled")
    );
}
