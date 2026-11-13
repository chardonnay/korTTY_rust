use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, mpsc};

use sevenz_rust2::{ArchiveReader, EncoderMethod, Password};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::scheduler_window::ArchiveFormat;
use crate::sftp_window::worker::{Control, Reply, Worker, sync::Cancel};
use crate::sftp_window::{Entry, EntryName, files};
use crate::snippets::tests::Profile;

use super::super::Options;
use super::{Plan, Request, apply, prepare};

fn request(
    root: &Path,
    selection: &[&str],
    destination: &Path,
    format: ArchiveFormat,
    level: u8,
    excludes: &[&str],
) -> Request {
    let listing = files::list_local(root).unwrap();
    let selected: Vec<Entry> = selection
        .iter()
        .map(|name| {
            listing
                .entries
                .iter()
                .find(|entry| matches!(&entry.name, EntryName::Local(value) if value == OsStr::new(name)))
                .unwrap()
                .clone()
        })
        .collect();
    Request::new(
        root.to_owned(),
        selected,
        Options {
            name: destination.to_str().unwrap().to_owned(),
            format,
            level,
            excludes: excludes
                .iter()
                .map(|pattern| (*pattern).to_owned())
                .collect(),
        },
    )
    .unwrap()
}

fn review_rows(plan: &Plan) -> BTreeMap<&str, bool> {
    let rows: BTreeMap<_, _> = plan
        .entries()
        .iter()
        .map(|(name, directory)| (name.as_str(), *directory))
        .collect();
    assert_eq!(rows.len(), plan.entries().len());
    rows
}

#[test]
fn local_browser_archives_review_and_write_the_java_zip_and_tar_layouts() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let tree = root.join("tree");
    let nested = tree.join("nested");
    let second = root.join("second");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(tree.join("empty")).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(nested.join("payload.txt"), b"checked payload").unwrap();
    fs::write(nested.join("skip.tmp"), b"excluded").unwrap();
    fs::write(second.join("notes.txt"), b"other root").unwrap();
    fs::write(root.join("loose.txt"), b"separate file").unwrap();
    fs::write(root.join("unselected.txt"), b"must not be read").unwrap();
    let selected = ["tree", "second", "loose.txt"];
    let payloads = [
        ("loose.txt", b"separate file".as_slice()),
        ("nested/payload.txt", b"checked payload".as_slice()),
        ("notes.txt", b"other root".as_slice()),
    ];
    let input_bytes: u64 = payloads.iter().map(|(_, bytes)| bytes.len() as u64).sum();

    let zip_path = root.join("selected.zip");
    let zip_review = prepare(
        request(
            &root,
            &selected,
            &zip_path,
            ArchiveFormat::Zip,
            6,
            &["*.tmp"],
        ),
        &Cancel::default(),
    )
    .unwrap();
    assert_eq!(zip_review.format(), ArchiveFormat::Zip);
    assert_eq!(zip_review.input_bytes(), input_bytes);
    assert_eq!(zip_review.excluded_count(), 1);
    assert_eq!(
        review_rows(&zip_review),
        BTreeMap::from(payloads.map(|(name, _)| (name, false)))
    );
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        4,
        "review is read-only"
    );
    assert!(matches!(
        apply(&zip_review, None, &Cancel::default()).unwrap(),
        Reply::Changed { .. }
    ));

    let mut zip = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
    assert_eq!(zip.len(), payloads.len());
    for (name, expected) in payloads {
        let mut entry = zip.by_name(name).unwrap();
        assert_eq!(entry.compression(), zip::CompressionMethod::Deflated);
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }

    let tar_path = root.join("selected.tar.bz2");
    let tar_review = prepare(
        request(
            &root,
            &selected,
            &tar_path,
            ArchiveFormat::TarBz2,
            6,
            &["*.tmp"],
        ),
        &Cancel::default(),
    )
    .unwrap();
    let expected_tar = [
        ("loose.txt", false, b"separate file".as_slice()),
        ("second", true, b"".as_slice()),
        ("second/notes.txt", false, b"other root".as_slice()),
        ("tree", true, b"".as_slice()),
        ("tree/empty", true, b"".as_slice()),
        ("tree/nested", true, b"".as_slice()),
        (
            "tree/nested/payload.txt",
            false,
            b"checked payload".as_slice(),
        ),
    ];
    assert_eq!(tar_review.format(), ArchiveFormat::TarBz2);
    assert_eq!(tar_review.input_bytes(), input_bytes);
    assert_eq!(tar_review.excluded_count(), 1);
    assert_eq!(
        review_rows(&tar_review),
        BTreeMap::from(expected_tar.map(|(name, directory, _)| (name, directory)))
    );
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        5,
        "review is read-only"
    );
    assert!(matches!(
        apply(&tar_review, None, &Cancel::default()).unwrap(),
        Reply::Changed { .. }
    ));

    let decoder = bzip2::read::BzDecoder::new(File::open(tar_path).unwrap());
    let mut archive = tar::Archive::new(decoder);
    let mut members = BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = std::str::from_utf8(&entry.path_bytes()).unwrap().to_owned();
        let directory = entry.header().entry_type().is_dir();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(members.insert(name, (directory, bytes)).is_none());
    }
    assert_eq!(members.len(), expected_tar.len());
    for (name, directory, bytes) in expected_tar {
        let (actual_directory, actual_bytes) = &members[name];
        assert_eq!(
            (*actual_directory, actual_bytes.as_slice()),
            (directory, bytes)
        );
    }
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        6,
        "no staging remains"
    );
}

#[test]
fn local_browser_archive_encrypts_zip_with_the_one_use_password() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let payload = b"local payload held in the AES archive";
    let password = "a private test password";
    fs::write(root.join("payload.txt"), payload).unwrap();
    let destination = root.join("encrypted.zip");
    let preview = || {
        prepare(
            request(
                &root,
                &["payload.txt"],
                &destination,
                ArchiveFormat::ZipPassword,
                0,
                &[],
            ),
            &Cancel::default(),
        )
        .unwrap()
    };
    assert!(apply(&preview(), None, &Cancel::default()).is_err());
    assert!(!destination.exists());

    let plan = preview();
    let Reply::Changed { note, .. } = apply(&plan, Some(password), &Cancel::default()).unwrap()
    else {
        panic!("expected completed AES ZIP publication");
    };
    assert!(!note.contains(password));
    let archive_bytes = fs::read(&destination).unwrap();
    assert!(
        !archive_bytes
            .windows(payload.len())
            .any(|bytes| bytes == payload)
    );
    assert!(
        !archive_bytes
            .windows(password.len())
            .any(|bytes| bytes == password.as_bytes())
    );
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&archive_bytes)).unwrap();
    assert_eq!(archive.len(), 1);
    assert_eq!(
        archive
            .get_aes_verification_key_and_salt(0)
            .unwrap()
            .unwrap()
            .aes_mode,
        zip::AesMode::Aes256
    );
    assert!(archive.by_name("payload.txt").is_err());
    let wrong_password = archive
        .by_name_decrypt("payload.txt", b"wrong password")
        .and_then(|mut entry| Ok(entry.read_to_end(&mut Vec::new())?));
    assert!(wrong_password.is_err());
    let mut entry = archive
        .by_name_decrypt("payload.txt", password.as_bytes())
        .unwrap();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.as_slice(), payload);
    assert!(apply(&plan, Some(password), &Cancel::default()).is_err());
    assert_eq!(fs::read(&destination).unwrap(), archive_bytes);
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        2,
        "no staging remains"
    );
}

#[test]
fn local_browser_archives_create_copy_and_lzma2_seven_zip() {
    const PAYLOAD_MEMBER: &str = "tree/nested/payload.txt";

    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let tree = root.join("tree");
    let nested = tree.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(tree.join("empty")).unwrap();
    let payload = b"checked local 7z payload\n".repeat(1024);
    fs::write(nested.join("payload.txt"), &payload).unwrap();
    fs::write(nested.join("empty-file"), b"").unwrap();
    fs::write(nested.join("skip.tmp"), b"excluded").unwrap();
    fs::write(root.join("loose.txt"), b"separate selected root").unwrap();
    fs::write(root.join("unselected.txt"), b"must not be read").unwrap();

    let expected = BTreeMap::from([
        ("loose.txt", false),
        ("tree", true),
        ("tree/empty", true),
        ("tree/nested", true),
        ("tree/nested/empty-file", false),
        (PAYLOAD_MEMBER, false),
    ]);
    for (name, level, method) in [
        ("copy.7z", 0, EncoderMethod::COPY),
        ("compressed.7z", 6, EncoderMethod::LZMA2),
    ] {
        let path = root.join(name);
        let before = fs::read_dir(&root).unwrap().count();
        let plan = prepare(
            request(
                &root,
                &["tree", "loose.txt"],
                &path,
                ArchiveFormat::SevenZip,
                level,
                &["*.tmp"],
            ),
            &Cancel::default(),
        )
        .unwrap();
        assert_eq!(plan.format(), ArchiveFormat::SevenZip);
        assert_eq!(plan.excluded_count(), 1);
        assert_eq!(
            plan.input_bytes(),
            (payload.len() + b"separate selected root".len()) as u64
        );
        assert_eq!(review_rows(&plan), expected);
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            before,
            "review is read-only"
        );
        assert!(matches!(
            apply(&plan, None, &Cancel::default()).unwrap(),
            Reply::Changed { .. }
        ));

        let mut archive = ArchiveReader::open(&path, Password::empty()).unwrap();
        let members = &archive.archive().files;
        assert_eq!(members.len(), expected.len());
        assert_eq!(
            members
                .iter()
                .map(|entry| (entry.name(), entry.is_directory()))
                .collect::<BTreeMap<_, _>>(),
            expected,
        );
        let mut methods = Vec::new();
        archive
            .file_compression_methods(PAYLOAD_MEMBER, &mut methods)
            .unwrap();
        assert_eq!(methods, [method]);
        assert_eq!(archive.read_file(PAYLOAD_MEMBER).unwrap(), payload);
        assert!(
            archive
                .read_file("tree/nested/empty-file")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            archive.read_file("loose.txt").unwrap().as_slice(),
            b"separate selected root"
        );
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            before + 1,
            "no staging remains"
        );
        check_independent_seven_zip(&path, &expected, PAYLOAD_MEMBER, &payload);
    }
}

fn check_independent_seven_zip(
    path: &Path,
    expected: &BTreeMap<&str, bool>,
    member: &str,
    payload: &[u8],
) {
    // Set this to a libarchive tar executable, such as Windows tar.exe or bsdtar.
    let Some(executable) = std::env::var_os("KORTTY_TEST_7Z_LIST") else {
        return;
    };
    let tar = |flags: &str| {
        let mut command = std::process::Command::new(&executable);
        command
            .current_dir(path.parent().unwrap())
            .arg(flags)
            .arg(path.file_name().unwrap());
        command
    };
    let output = tar("-tf").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let list = String::from_utf8(output.stdout).unwrap();
    let mut names: Vec<_> = list
        .lines()
        .map(|line| line.trim_end_matches('/'))
        .collect();
    names.sort_unstable();
    assert_eq!(names, expected.keys().copied().collect::<Vec<_>>());

    let contents = tar("-xOf").arg(member).output().unwrap();
    assert!(
        contents.status.success(),
        "{}",
        String::from_utf8_lossy(&contents.stderr)
    );
    assert_eq!(contents.stdout, payload);
}

const SEVEN_ZIP_TEST_PASSWORD: &str = "Ärchiv-grüß-密碼-🔒";
const SEVEN_ZIP_WRONG_PASSWORD: &str = "wrong synthetic password";

#[test]
fn local_browser_archive_encrypts_stored_seven_zip_members_and_hides_names() {
    verify_encrypted_seven_zip(0);
}

#[test]
fn local_browser_archive_encrypts_compressed_seven_zip_members_and_hides_names() {
    verify_encrypted_seven_zip(6);
}

fn verify_encrypted_seven_zip(level: u8) {
    const FIRST: &str = "private-folder/first-payload.txt";
    const SECOND: &str = "private-folder/second-payload.txt";
    const FIRST_DATA: &[u8] = b"first synthetic payload with distinctive plaintext";
    const SECOND_DATA: &[u8] = b"second synthetic payload with different plaintext";

    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let source = root.join("private-folder");
    fs::create_dir(&source).unwrap();
    fs::create_dir(source.join("empty-child")).unwrap();
    fs::write(source.join("first-payload.txt"), FIRST_DATA).unwrap();
    fs::write(source.join("second-payload.txt"), SECOND_DATA).unwrap();
    fs::write(source.join("excluded.tmp"), b"excluded").unwrap();
    let path = root.join("encrypted.7z");
    let preview = || {
        prepare(
            request(
                &root,
                &["private-folder"],
                &path,
                ArchiveFormat::SevenZipPassword,
                level,
                &["*.tmp"],
            ),
            &Cancel::default(),
        )
        .unwrap()
    };
    assert!(apply(&preview(), None, &Cancel::default()).is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

    let expected = BTreeMap::from([
        ("private-folder", true),
        ("private-folder/empty-child", true),
        (FIRST, false),
        (SECOND, false),
    ]);
    let plan = preview();
    assert_eq!(plan.format(), ArchiveFormat::SevenZipPassword);
    assert_eq!(
        plan.input_bytes(),
        (FIRST_DATA.len() + SECOND_DATA.len()) as u64
    );
    assert_eq!(plan.excluded_count(), 1);
    assert_eq!(review_rows(&plan), expected);
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        1,
        "review is read-only"
    );
    let Reply::Changed { note, .. } =
        apply(&plan, Some(SEVEN_ZIP_TEST_PASSWORD), &Cancel::default()).unwrap()
    else {
        panic!("expected completed encrypted 7z publication");
    };
    assert!(!note.contains(SEVEN_ZIP_TEST_PASSWORD));

    let bytes = fs::read(&path).unwrap();
    for payload in [FIRST_DATA, SECOND_DATA] {
        assert!(!bytes.windows(payload.len()).any(|window| window == payload));
    }
    for name in [
        "private-folder",
        "empty-child",
        "first-payload.txt",
        "second-payload.txt",
        SEVEN_ZIP_TEST_PASSWORD,
    ] {
        assert_seven_zip_hides(&bytes, name);
    }

    let mut reader = open_encrypted_seven_zip(&path);
    assert_eq!(encrypted_seven_zip_members(&reader), expected);
    assert_distinct_seven_zip_ivs(&reader);
    assert_eq!(reader.read_file(FIRST).unwrap(), FIRST_DATA);
    assert_eq!(reader.read_file(SECOND).unwrap(), SECOND_DATA);
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        2,
        "no staging remains"
    );
    check_independent_encrypted_seven_zip(&path, &expected, Some((FIRST, FIRST_DATA)));
}

#[test]
fn local_browser_archive_encrypts_directory_only_seven_zip_header() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let folder = root.join("private-folders-only");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(folder.join("hidden-empty-child")).unwrap();
    let path = root.join("folders.7z");
    let plan = prepare(
        request(
            &root,
            &["private-folders-only"],
            &path,
            ArchiveFormat::SevenZipPassword,
            6,
            &[],
        ),
        &Cancel::default(),
    )
    .unwrap();
    assert_eq!(plan.input_bytes(), 0);
    let expected = BTreeMap::from([
        ("private-folders-only", true),
        ("private-folders-only/hidden-empty-child", true),
    ]);
    assert_eq!(review_rows(&plan), expected);
    assert!(matches!(
        apply(&plan, Some(SEVEN_ZIP_TEST_PASSWORD), &Cancel::default()).unwrap(),
        Reply::Changed { .. }
    ));
    let bytes = fs::read(&path).unwrap();
    assert_seven_zip_hides(&bytes, "private-folders-only");
    assert_seven_zip_hides(&bytes, "hidden-empty-child");
    let reader = open_encrypted_seven_zip(&path);
    assert_eq!(encrypted_seven_zip_members(&reader), expected);
    assert!(
        reader
            .archive()
            .files
            .iter()
            .all(|entry| !entry.has_stream())
    );
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        2,
        "no staging remains"
    );
    check_independent_encrypted_seven_zip(&path, &expected, None);
}

fn assert_seven_zip_hides(bytes: &[u8], text: &str) {
    let utf16: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert!(
        !bytes
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    );
    assert!(
        !bytes
            .windows(utf16.len())
            .any(|window| window == utf16.as_slice())
    );
}

fn open_encrypted_seven_zip(path: &Path) -> ArchiveReader<File> {
    assert!(ArchiveReader::open(path, Password::empty()).is_err());
    assert!(ArchiveReader::open(path, Password::new(SEVEN_ZIP_WRONG_PASSWORD)).is_err());
    ArchiveReader::open(path, Password::new(SEVEN_ZIP_TEST_PASSWORD)).unwrap()
}

fn encrypted_seven_zip_members(reader: &ArchiveReader<File>) -> BTreeMap<&str, bool> {
    let archive = reader.archive();
    let members: BTreeMap<_, _> = archive
        .files
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            if entry.has_stream() {
                assert!(archive.is_file_encrypted(index));
            }
            (entry.name(), entry.is_directory())
        })
        .collect();
    assert_eq!(members.len(), archive.files.len());
    members
}

fn assert_distinct_seven_zip_ivs(reader: &ArchiveReader<File>) {
    let ivs: Vec<_> = reader
        .archive()
        .blocks
        .iter()
        .flat_map(|block| &block.coders)
        .filter(|coder| coder.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
        .map(|coder| {
            let properties = coder.properties();
            // The two AES property bytes encode the KDF exponent and salt and IV lengths.
            assert_eq!(properties[0] & 0x3f, 19);
            let salt_len = usize::from((properties[0] >> 7) & 1) + usize::from(properties[1] >> 4);
            let iv_len = usize::from((properties[0] >> 6) & 1) + usize::from(properties[1] & 15);
            assert_eq!(salt_len, 16);
            assert_eq!(iv_len, 16);
            assert_eq!(properties.len(), 2 + salt_len + iv_len);
            &properties[2 + salt_len..]
        })
        .collect();
    assert_eq!(ivs.len(), 2);
    assert_ne!(ivs[0], ivs[1]);
}

fn check_independent_encrypted_seven_zip(
    path: &Path,
    expected: &BTreeMap<&str, bool>,
    sample: Option<(&str, &[u8])>,
) {
    let Some(executable) = std::env::var_os("KORTTY_TEST_7ZZ") else {
        return;
    };
    // Only these synthetic test passwords are passed as arguments to 7-Zip.
    let seven_zip = |action: &str, password: &'static str, flags: &[&str]| {
        let mut command = std::process::Command::new(&executable);
        command
            .current_dir(path.parent().unwrap())
            .stdin(std::process::Stdio::null())
            .arg(action)
            .arg(format!("-p{password}"))
            .args(["-bd", "-sccUTF-8"])
            .args(flags)
            .arg("--")
            .arg(path.file_name().unwrap());
        command
    };
    let refused = seven_zip("l", SEVEN_ZIP_WRONG_PASSWORD, &["-slt"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let refused = String::from_utf8_lossy(&refused.stdout);
    assert!(expected.keys().all(|name| !refused.contains(*name)));

    let listed = seven_zip("l", SEVEN_ZIP_TEST_PASSWORD, &["-slt"])
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listing = String::from_utf8(listed.stdout).unwrap();
    let mut members: Vec<_> = listing
        .lines()
        .skip_while(|line| *line != "----------")
        .skip(1)
        .filter_map(|line| line.strip_prefix("Path = "))
        .map(|name| name.replace('\\', "/"))
        .collect();
    members.sort_unstable();
    assert_eq!(members, expected.keys().copied().collect::<Vec<_>>());

    if let Some((member, payload)) = sample {
        let contents = seven_zip("x", SEVEN_ZIP_TEST_PASSWORD, &["-so", "-spd", "-y"])
            .arg(member)
            .output()
            .unwrap();
        assert!(
            contents.status.success(),
            "{}",
            String::from_utf8_lossy(&contents.stderr)
        );
        assert_eq!(contents.stdout, payload);
    }
}

#[test]
fn local_browser_zip_rejects_collisions_between_normalized_selected_roots() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    for (directory, content) in [
        ("left", b"left file".as_slice()),
        ("right", b"right file".as_slice()),
    ] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("same.txt"), content).unwrap();
    }
    let destination = root.join("collision.zip");
    let error = prepare(
        request(
            &root,
            &["left", "right"],
            &destination,
            ArchiveFormat::Zip,
            6,
            &[],
        ),
        &Cancel::default(),
    )
    .unwrap_err();
    assert!(!destination.exists(), "{error:?}");
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        2,
        "no staging remains"
    );
}

#[test]
fn local_browser_archive_rechecks_source_destination_and_cancellation() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let tree = root.join("tree");
    let target = root.join("target");
    let moved = root.join("moved");
    fs::create_dir(&tree).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(tree.join("payload"), b"approved").unwrap();
    let destination = target.join("archive.zip");
    let preview = || {
        prepare(
            request(&root, &["tree"], &destination, ArchiveFormat::Zip, 6, &[]),
            &Cancel::default(),
        )
        .unwrap()
    };

    let stale_source = preview();
    fs::write(tree.join("new"), b"after review").unwrap();
    assert!(apply(&stale_source, None, &Cancel::default()).is_err());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    assert_eq!(fs::read(tree.join("new")).unwrap(), b"after review");

    let stale_destination = preview();
    fs::rename(&target, &moved).unwrap();
    fs::create_dir(&target).unwrap();
    assert!(apply(&stale_destination, None, &Cancel::default()).is_err());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);

    let (commands, _incoming) = mpsc::sync_channel(1);
    let (_completed, done) = async_channel::bounded(1);
    let worker = Worker {
        commands,
        control: Arc::new(Control::default()),
        done,
        editor_policy: RemoteEditorPolicy::Deny,
        exec: None,
        sftp: None,
    };
    let cancelled = preview();
    let cancel = Cancel::default();
    cancel.stop(&worker);
    assert!(apply(&cancelled, None, &cancel).is_err());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);

    let raced = preview();
    fs::write(&destination, b"concurrent file").unwrap();
    assert!(apply(&raced, None, &Cancel::default()).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"concurrent file");
    assert_eq!(
        fs::read_dir(&target).unwrap().count(),
        1,
        "no staging remains"
    );
    assert_eq!(fs::read(tree.join("payload")).unwrap(), b"approved");
}

#[cfg(unix)]
#[test]
fn local_browser_archive_refuses_a_link_inserted_after_review() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let tree = root.join("tree");
    fs::create_dir(&tree).unwrap();
    let payload = tree.join("payload");
    fs::write(&payload, b"approved").unwrap();
    let outside = root.join("outside");
    fs::write(&outside, b"outside selection").unwrap();
    let destination = root.join("archive.zip");
    let plan = prepare(
        request(&root, &["tree"], &destination, ArchiveFormat::Zip, 6, &[]),
        &Cancel::default(),
    )
    .unwrap();
    fs::remove_file(&payload).unwrap();
    std::os::unix::fs::symlink(&outside, &payload).unwrap();
    assert!(apply(&plan, None, &Cancel::default()).is_err());
    assert!(!destination.exists());
    assert!(payload.is_symlink());
    assert_eq!(fs::read(&outside).unwrap(), b"outside selection");
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        2,
        "no staging remains"
    );
}
