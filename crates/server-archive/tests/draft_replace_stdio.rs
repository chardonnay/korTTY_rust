#![cfg(all(
    feature = "decoder",
    any(target_os = "linux", target_os = "macos", windows)
))]

use std::fs;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use kortty_server_archive::draft_replace::{
    ARGUMENT, Original, Reply, Request, Spec, read_reply, write_request,
};
use sha2::{Digest as _, Sha256};

const ORIGINAL: &[u8] = b"\xef\xbb\xbfkeep the raw original\r\n";
const DRAFT: &[u8] = b"reviewed new content\n";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exchange(request: &Request, draft: &[u8], trailing: &[u8]) -> Reply {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kortty-server-archive"))
        .arg(ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    write_request(&mut input, request, draft).unwrap();
    input.write_all(trailing).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    read_reply(output.stdout.as_slice()).unwrap()
}

#[test]
fn draft_replace_stdio_requires_exact_draft_and_eof_and_publishes_once() {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("kortty-draft-stdio-{}", hex::encode(nonce)));
    #[cfg(unix)]
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    #[cfg(windows)]
    kortty_files::tree::Directory::open_path(root.parent().unwrap())
        .unwrap()
        .create_child(root.file_name().unwrap())
        .unwrap();
    let root = Scratch(root);
    let path = root.0.join("original.txt");
    fs::write(&path, ORIGINAL).unwrap();
    #[cfg(unix)]
    let (destination, uid, gid, mode) = {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        (
            path.to_str().unwrap().to_owned(),
            metadata.uid(),
            metadata.gid(),
            0o640,
        )
    };
    #[cfg(windows)]
    let (destination, uid, gid, mode) = (
        format!(
            "/{}",
            path.to_str()
                .unwrap()
                .trim_start_matches(r"\\?\")
                .replace('\\', "/")
        ),
        0,
        0,
        0,
    );
    let spec = Spec {
        destination,
        original: Original {
            bytes: ORIGINAL.len() as u64,
            digest: Sha256::digest(ORIGINAL).into(),
            uid,
            gid,
            mode,
        },
        draft_bytes: DRAFT.len(),
        draft_digest: Sha256::digest(DRAFT).into(),
        backup: true,
    };
    let response = exchange(&Request::Review { spec: spec.clone() }, &[], &[]);
    let Reply::Reviewed { review } = response else {
        panic!("expected a server draft review, received {response:?}");
    };
    review.validate_for(&spec).unwrap();
    let apply = Request::Apply {
        spec: spec.clone(),
        token: review.token,
    };
    assert!(matches!(
        exchange(&apply, DRAFT, b"extra"),
        Reply::Failed {
            outcome_unknown: false,
            ..
        }
    ));
    assert_eq!(fs::read(&path).unwrap(), ORIGINAL);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    let result = exchange(&apply, DRAFT, &[]);
    assert!(
        matches!(result, Reply::Applied { recovery: None, .. }),
        "{result:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), DRAFT);
    assert_eq!(fs::read(root.0.join("original.txt.bak")).unwrap(), ORIGINAL);
    assert!(matches!(
        exchange(&apply, DRAFT, &[]),
        Reply::Failed {
            outcome_unknown: false,
            ..
        }
    ));
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
}
