#![cfg(all(feature = "decoder", target_os = "linux"))]

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use kortty_server_archive::protocol::{
    CreateFormat, CreateSpec, PRIVILEGED_ARGUMENT, PRIVILEGED_PATH, PrivilegedReply,
    PrivilegedRequest, read_privileged_reply, write_privileged_request,
};

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
    };
    let zip = spec("zip", CreateFormat::Zip);

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
    assert_eq!(fs::read_dir("/data/output").unwrap().count(), 4);
}

struct Helper {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Helper {
    fn open() -> Self {
        let mut child = Command::new(PRIVILEGED_PATH)
            .arg(PRIVILEGED_ARGUMENT)
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
