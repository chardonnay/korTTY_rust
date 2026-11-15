#![cfg(all(feature = "decoder", target_os = "linux"))]

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use kortty_server_archive::protocol::{
    CreateFormat, CreateSpec, OutputMetadata, PASSWORD_COMMIT, PRIVILEGED_ARGUMENT,
    PRIVILEGED_PASSWORD_ARGUMENT, PRIVILEGED_PASSWORD_COMMIT, PRIVILEGED_PASSWORD_READY,
    PRIVILEGED_PATH, PrivilegedReply, PrivilegedRequest, creation_password_body,
    read_privileged_reply, write_privileged_request,
};
use sha2::{Digest, Sha256};

const CONFIG: &str = "/etc/kortty/privileged-archive.json";
const RULES: &str = r#"{"source_roots":["/data/source"],"destination_roots":["/data/output"]}"#;

// Run only in a disposable namespace with the tested helper at its fixed path.
// Keep /data, /etc/kortty, and /usr/libexec/kortty on namespace-owned storage.
#[test]
#[ignore = "requires a disposable root-mapped Linux namespace and an installed fixture helper"]
fn privileged_archive_stdio_scopes_one_commit_and_preserves_existing_paths() {
    assert_eq!(
        std::env::var("KORTTY_ARCHIVE_TEST_ROOT").as_deref(),
        Ok("/data")
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    assert!(!std::path::Path::new(CONFIG).exists());
    assert!(fs::read_dir("/data").unwrap().next().is_none());
    fs::create_dir_all("/etc/kortty").unwrap();
    fs::write(CONFIG, RULES).unwrap();
    fs::create_dir("/data/source").unwrap();
    fs::create_dir("/data/output").unwrap();
    fs::create_dir("/data/source/empty").unwrap();
    fs::create_dir("/data/source-other").unwrap();
    fs::write("/data/source/example.txt", b"protected archive payload").unwrap();
    fs::write(
        "/data/source-other/blocked.txt",
        b"outside the allowed root",
    )
    .unwrap();
    let spec = |suffix: &str, format| CreateSpec {
        sources: vec!["/data/source".into()],
        destination: format!("/data/output/archive.{suffix}"),
        format,
        level: 3,
        excludes: vec![],
        output: None,
        report_payload: false,
    };
    let zip = spec("zip", CreateFormat::Zip);

    if std::env::var_os("KORTTY_ARCHIVE_TEST_USE_SUDO").is_none() {
        let separate = spec("greeting.tar", CreateFormat::Tar);
        let mut disconnected = Helper::open_password_protocol();
        let token = disconnected.review(&separate);
        assert_failed(disconnected.complete(&PrivilegedRequest::Commit { token }));
        assert!(!std::path::Path::new(&separate.destination).exists());
        let mut helper = Helper::open_password_protocol();
        let token = helper.review(&separate);
        helper.send(&PrivilegedRequest::Commit { token });
        helper
            .input
            .as_mut()
            .unwrap()
            .write_all(PRIVILEGED_PASSWORD_COMMIT)
            .unwrap();
        assert!(matches!(
            helper.finish(),
            PrivilegedReply::Applied { destination } if destination == separate.destination
        ));
        fs::remove_file(&separate.destination).unwrap();
    }

    let mut output = zip.clone();
    output.output = Some(OutputMetadata {
        uid: Some(1),
        gid: None,
        mode: Some(0o644),
    });
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review { spec: output }));
    assert!(!std::path::Path::new(&zip.destination).exists());
    let mut download = zip.clone();
    download.report_payload = true;
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review { spec: download }));
    assert!(!std::path::Path::new(&zip.destination).exists());

    let mut outside = zip.clone();
    outside.sources = vec!["/data/source-other/blocked.txt".into()];
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review { spec: outside }));
    let mut wrong_destination = zip.clone();
    wrong_destination.destination = "/data/archive.zip".into();
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review {
        spec: wrong_destination,
    }));
    symlink("/data/source-other/blocked.txt", "/data/source/link.txt").unwrap();
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review { spec: zip.clone() }));
    fs::remove_file("/data/source/link.txt").unwrap();
    fs::hard_link("/data/source/example.txt", "/data/source/link.txt").unwrap();
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review { spec: zip.clone() }));
    fs::remove_file("/data/source/link.txt").unwrap();

    let mut first = Helper::open();
    let stale = first.review(&zip);
    assert!(matches!(
        first.complete(&PrivilegedRequest::Abort),
        PrivilegedReply::Aborted
    ));
    let mut second = Helper::open();
    assert_ne!(second.review(&zip), stale);
    assert_failed(second.complete(&PrivilegedRequest::Commit { token: stale }));
    assert!(!std::path::Path::new(&zip.destination).exists());

    let mut dropped = Helper::open();
    dropped.review(&zip);
    assert!(matches!(dropped.finish(), PrivilegedReply::Aborted));
    let mut extra = Helper::open();
    let token = extra.review(&zip);
    extra.send(&PrivilegedRequest::Commit { token });
    extra.input.as_mut().unwrap().write_all(b"extra").unwrap();
    assert_failed(extra.finish());
    let mut edited = Helper::open();
    let token = edited.review(&zip);
    fs::write("/data/source/example.txt", b"changed").unwrap();
    assert_failed(edited.complete(&PrivilegedRequest::Commit { token }));
    let mut collided = Helper::open();
    let token = collided.review(&zip);
    fs::write(&zip.destination, b"existing").unwrap();
    assert_failed(collided.complete(&PrivilegedRequest::Commit { token }));
    assert_eq!(fs::read(&zip.destination).unwrap(), b"existing");
    fs::remove_file(&zip.destination).unwrap();

    let mut revoked = Helper::open();
    let token = revoked.review(&zip);
    fs::write(
        CONFIG,
        r#"{"source_roots":["/data/source-other"],"destination_roots":["/data/output"]}"#,
    )
    .unwrap();
    assert_failed(revoked.complete(&PrivilegedRequest::Commit { token }));
    fs::write(CONFIG, RULES).unwrap();

    for (suffix, format) in [
        ("zip", CreateFormat::Zip),
        ("tar", CreateFormat::Tar),
        ("tar.bz2", CreateFormat::TarBzip2),
        ("7z", CreateFormat::SevenZip),
    ] {
        let spec = spec(suffix, format);
        let mut helper = Helper::open();
        let token = helper.review(&spec);
        assert!(
            matches!(helper.complete(&PrivilegedRequest::Commit { token }), PrivilegedReply::Applied { destination } if destination == spec.destination)
        );
        assert_eq!(
            fs::metadata(&spec.destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let (format, _) = kortty_server_archive::Format::detect(&spec.destination).unwrap();
        let mut payload = None;
        let manifest = kortty_server_archive::decode::visit(
            format,
            &fs::read(spec.destination).unwrap(),
            |member, bytes| {
                if member.path == "source/example.txt" {
                    let mut content = Vec::new();
                    bytes.read_to_end(&mut content)?;
                    payload = Some(content);
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(payload.unwrap(), b"changed");
        assert!(
            manifest
                .members
                .iter()
                .any(|member| member.path == "source/empty")
        );
    }
    let mut protected = spec("zip", CreateFormat::ZipPassword);
    protected.destination = "/data/output/password.zip".into();
    const PASSWORD: &str = "fixture password\n東京";
    let body = creation_password_body(PASSWORD).unwrap();
    for marker in [
        &[][..],
        &PASSWORD_COMMIT[..PASSWORD_COMMIT.len() - 1],
        b"wrong",
    ] {
        let mut helper = Helper::open();
        let token = helper.review(&protected);
        helper.send(&PrivilegedRequest::Commit { token });
        let input = helper.input.as_mut().unwrap();
        input.write_all(&body).unwrap();
        input.write_all(marker).unwrap();
        assert_failed(helper.finish());
        assert!(!std::path::Path::new(&protected.destination).exists());
    }
    let mut helper = Helper::open();
    let token = helper.review(&protected);
    helper.send(&PrivilegedRequest::Commit { token });
    helper.input.as_mut().unwrap().write_all(&body).unwrap();
    helper
        .input
        .as_mut()
        .unwrap()
        .write_all(PASSWORD_COMMIT)
        .unwrap();
    assert!(
        matches!(helper.finish(), PrivilegedReply::Applied { destination } if destination == protected.destination)
    );
    let metadata = fs::metadata(&protected.destination).unwrap();
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.mode() & 0o777, 0o600);
    let mut archive =
        zip::ZipArchive::new(fs::File::open(&protected.destination).unwrap()).unwrap();
    assert!(archive.by_name("source/example.txt").is_err());
    assert!(
        archive
            .by_name_decrypt("source/example.txt", b"incorrect")
            .is_err()
    );
    let mut contents = Vec::new();
    archive
        .by_name_decrypt("source/example.txt", PASSWORD.as_bytes())
        .unwrap()
        .read_to_end(&mut contents)
        .unwrap();
    assert_eq!(contents, b"changed");
    assert_eq!(fs::read_dir("/data/output").unwrap().count(), 5);

    let download_rules = r#"{"source_roots":["/data/source"],"destination_roots":["/data/output"],"download_roots":["/data/output/downloadable"]}"#;
    fs::create_dir("/data/output/downloadable").unwrap();
    fs::write(CONFIG, download_rules).unwrap();
    let mut copy = protected;
    copy.report_payload = true;
    copy.destination = "/data/output/not-downloadable.zip".into();
    assert_failed(Helper::open().complete(&PrivilegedRequest::Review { spec: copy.clone() }));
    assert!(!std::path::Path::new(&copy.destination).exists());

    copy.destination = "/data/output/downloadable/bytes.zip".into();
    let mut revoked = Helper::open();
    let token = revoked.review(&copy);
    fs::write(CONFIG, RULES).unwrap();
    revoked.send(&PrivilegedRequest::Commit { token });
    revoked.input.as_mut().unwrap().write_all(&body).unwrap();
    revoked
        .input
        .as_mut()
        .unwrap()
        .write_all(PASSWORD_COMMIT)
        .unwrap();
    assert_failed(revoked.finish());
    assert!(!std::path::Path::new(&copy.destination).exists());

    fs::write(CONFIG, download_rules).unwrap();
    let mut helper = Helper::open();
    let token = helper.review(&copy);
    helper.send(&PrivilegedRequest::Commit { token });
    helper.input.as_mut().unwrap().write_all(&body).unwrap();
    helper
        .input
        .as_mut()
        .unwrap()
        .write_all(PASSWORD_COMMIT)
        .unwrap();
    helper.input.take();
    let PrivilegedReply::AppliedCopy {
        destination,
        payload,
    } = read_privileged_reply(&mut helper.output).unwrap()
    else {
        panic!("expected the verified contents of the newly published archive");
    };
    assert_eq!(destination, copy.destination);
    payload.validate().unwrap();
    let mut actual = vec![0; payload.bytes as usize];
    helper.output.read_exact(&mut actual).unwrap();
    assert_eq!(io::copy(&mut helper.output, &mut io::sink()).unwrap(), 0);
    assert!(helper.child.wait().unwrap().success());
    assert_eq!(payload.sha256, <[u8; 32]>::from(Sha256::digest(&actual)));
    assert_eq!(actual, fs::read(&destination).unwrap());
    let metadata = fs::metadata(&destination).unwrap();
    assert_eq!(
        (metadata.uid(), metadata.mode() & 0o777, metadata.nlink()),
        (0, 0o600, 1)
    );

    fs::write("/data/source/large.bin", vec![9; 256 * 1024]).unwrap();
    copy.destination = "/data/output/downloadable/revoked.tar".into();
    copy.format = CreateFormat::Tar;
    let mut helper = Helper::open();
    let token = helper.review(&copy);
    helper.send(&PrivilegedRequest::Commit { token });
    helper.input.take();
    assert!(matches!(
        read_privileged_reply(&mut helper.output).unwrap(),
        PrivilegedReply::AppliedCopy { .. }
    ));
    fs::write(CONFIG, RULES).unwrap();
    io::copy(&mut helper.output, &mut io::sink()).unwrap();
    assert!(!helper.child.wait().unwrap().success());
    assert!(fs::metadata(&copy.destination).unwrap().len() >= 256 * 1024);
}

struct Helper {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Helper {
    fn open() -> Self {
        Self::open_with_argument(PRIVILEGED_ARGUMENT)
    }

    fn open_password_protocol() -> Self {
        let mut helper = Self::open_with_argument(PRIVILEGED_PASSWORD_ARGUMENT);
        let mut greeting = [0; PRIVILEGED_PASSWORD_READY.len()];
        helper.output.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting.as_slice(), PRIVILEGED_PASSWORD_READY);
        helper
    }

    fn open_with_argument(argument: &str) -> Self {
        let mut command = if std::env::var_os("KORTTY_ARCHIVE_TEST_USE_SUDO").is_some() {
            let mut command = Command::new("/usr/bin/runuser");
            command.args([
                "-u",
                "packager",
                "--",
                "/usr/bin/sudo",
                "-k",
                "-n",
                "-u",
                "root",
                "--",
                PRIVILEGED_PATH,
            ]);
            command
        } else {
            Command::new(PRIVILEGED_PATH)
        };
        let mut child = command
            .arg(argument)
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

    fn send(&mut self, request: &PrivilegedRequest) {
        write_privileged_request(self.input.as_mut().unwrap(), request).unwrap();
    }

    fn review(&mut self, spec: &CreateSpec) -> String {
        self.send(&PrivilegedRequest::Review { spec: spec.clone() });
        let reply = read_privileged_reply(&mut self.output).unwrap();
        let PrivilegedReply::Review { review } = reply else {
            panic!("expected a review: {reply:?}")
        };
        review.validate_for(spec).unwrap();
        review.token
    }

    fn complete(mut self, request: &PrivilegedRequest) -> PrivilegedReply {
        self.send(request);
        self.finish()
    }

    fn finish(mut self) -> PrivilegedReply {
        self.input.take();
        let reply = read_privileged_reply(&mut self.output).unwrap();
        assert_eq!(io::copy(&mut self.output, &mut io::sink()).unwrap(), 0);
        assert!(self.child.wait().unwrap().success());
        reply
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn assert_failed(reply: PrivilegedReply) {
    assert!(
        matches!(
            reply,
            PrivilegedReply::Failed {
                outcome_unknown: false,
                ..
            }
        ),
        "{reply:?}"
    );
}
