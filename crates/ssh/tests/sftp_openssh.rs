//! Opt-in test against a local OpenSSH sftp-server, using an isolated temporary directory.
#![cfg(unix)]

use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use kortty_ssh::sftp::{Error, FileKind, Sftp};

mod support;
use support::TempDirectory;

#[test]
#[ignore = "requires an installed OpenSSH sftp-server"]
fn openssh_sftp_roundtrip_and_mutations() {
    let directory = TempDirectory::new("sftp");
    let mut server = Server::start(&directory.0);
    let output = server.0.stdout.take().unwrap();
    let input = server.0.stdin.take().unwrap();
    let mut client = Sftp::new(output, input).unwrap();

    assert_eq!(
        client.canonicalize(".").unwrap(),
        directory.0.canonicalize().unwrap().as_os_str().as_bytes()
    );
    assert!(client.stat("missing").unwrap_err().is_not_found());
    client.mkdir("files").unwrap();
    assert_eq!(client.stat("files").unwrap().kind(), FileKind::Directory);

    let payload: Vec<_> = (0..(130 * 1024 + 9)).map(|index| index as u8).collect();
    assert_eq!(
        client
            .upload_new("files/upload.part", payload.as_slice())
            .unwrap(),
        payload.len() as u64
    );
    let attributes = client.stat("files/upload.part").unwrap();
    assert_eq!(attributes.size, Some(payload.len() as u64));
    assert_eq!(attributes.permissions.unwrap() & 0o777, 0o600);
    client.rename("files/upload.part", "files/asset").unwrap();
    assert!(client.upload_new("files/asset", &b"changed"[..]).is_err());

    symlink("asset", directory.0.join("files/link")).unwrap();
    assert_eq!(
        client.lstat("files/link").unwrap().kind(),
        FileKind::Symlink
    );
    assert_eq!(client.stat("files/link").unwrap().kind(), FileKind::File);
    let entries = client.read_dir("files").unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == b"asset" && entry.attrs.kind() == FileKind::File)
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == b"link" && entry.attrs.kind() == FileKind::Symlink)
    );

    let mut downloaded = Vec::new();
    assert_eq!(
        client.download("files/asset", &mut downloaded).unwrap(),
        payload.len() as u64
    );
    assert_eq!(downloaded, payload);
    assert_eq!(
        client
            .upload_new("files/replace.part", &b"replacement"[..])
            .unwrap(),
        11
    );
    assert!(client.rename("files/replace.part", "files/asset").is_err());
    assert!(client.supports_atomic_replace());
    client.set_permissions("files/replace.part", 0o640).unwrap();
    client.replace("files/replace.part", "files/asset").unwrap();
    assert_eq!(
        client.stat("files/asset").unwrap().permissions.unwrap() & 0o777,
        0o640
    );
    assert_eq!(
        fs::read(directory.0.join("files/asset")).unwrap(),
        b"replacement"
    );

    client.remove_file("files/link").unwrap();
    client.remove_file("files/asset").unwrap();
    client.remove_dir("files").unwrap();
    assert!(client.read_dir(".").unwrap().is_empty());
    drop(client);
    assert!(server.0.wait().unwrap().success());
}

#[test]
#[ignore = "requires OpenSSH sftp-server with copy-data, hardlink, and lsetstat support"]
fn openssh_server_copy_preserves_existing_paths_and_accepts_omitted_gid() {
    let directory = TempDirectory::new("sftp-extensions");
    let mut server = Server::start(&directory.0);
    let output = server.0.stdout.take().unwrap();
    let input = server.0.stdin.take().unwrap();
    let mut client = Sftp::new(output, input).unwrap();

    let payload = b"a server-side payload\0";
    client.upload_new("source", &payload[..]).unwrap();
    let original = client.lstat("source").unwrap();
    client
        .set_owner_no_follow("source", Some(original.uid.unwrap()), None)
        .unwrap();
    let current = client.lstat("source").unwrap();
    assert_eq!((current.uid, current.gid), (original.uid, original.gid));
    assert_eq!(
        client
            .copy_regular_new(b"source", current, b"stage", || false)
            .unwrap(),
        payload.len() as u64
    );

    let existing = directory.0.join("target");
    fs::write(&existing, b"another writer's file").unwrap();
    assert!(client.hardlink("stage", "target").is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"another writer's file");
    client.remove_file("target").unwrap();
    symlink("missing", &existing).unwrap();
    assert!(client.hardlink("stage", "target").is_err());
    assert_eq!(fs::read_link(&existing).unwrap(), Path::new("missing"));
    assert!(!directory.0.join("missing").exists());
    client.remove_file("target").unwrap();
    client.hardlink("stage", "target").unwrap();
    client.remove_file("stage").unwrap();
    assert_eq!(fs::read(&existing).unwrap(), payload);

    let large = 8 * 1024 * 1024 + 1;
    fs::File::create(directory.0.join("large"))
        .unwrap()
        .set_len(large)
        .unwrap();
    let expected = client.lstat("large").unwrap();
    let interrupted = directory.0.join("interrupted");
    let result = client.copy_regular_new(b"large", expected, b"interrupted", || {
        fs::metadata(&interrupted).is_ok_and(|metadata| metadata.len() > 0)
    });
    assert!(
        matches!(result, Err(Error::LocalIo(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    let partial = client.lstat("interrupted").unwrap().size.unwrap();
    assert!((1..large).contains(&partial));
    client.remove_file("interrupted").unwrap();

    drop(client);
    assert!(server.0.wait().unwrap().success());
}

struct Server(Child);

impl Server {
    fn start(directory: &Path) -> Self {
        let executable = std::env::var_os("SFTP_SERVER")
            .map(PathBuf::from)
            .or_else(|| {
                [
                    "/usr/libexec/sftp-server",
                    "/usr/lib/openssh/sftp-server",
                    "/usr/lib/ssh/sftp-server",
                ]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
            });
        let child = Command::new(
            executable.expect("set SFTP_SERVER to the OpenSSH sftp-server executable"),
        )
        .arg("-d")
        .arg(directory)
        .args(["-u", "022"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
        Self(child)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
