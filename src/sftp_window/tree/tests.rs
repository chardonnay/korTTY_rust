use std::fs;
use std::path::PathBuf;

use super::*;

mod ordinary_download;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        let path = temporary.join(files::nonce().unwrap());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn local_directory_publication_refuses_collisions_and_cleans_staging() {
    let fixture = Fixture::new();
    let parent = local::Directory::open_path(&fixture.0).unwrap();
    let stage_name = OsStr::new("stage");
    let stage = parent.create_child(stage_name).unwrap();
    let inner = stage.create_child(OsStr::new("nested")).unwrap();
    inner
        .create_file(OsStr::new("asset"))
        .unwrap()
        .write_all(b"data")
        .unwrap();
    drop(inner);

    let dest = fixture.0.join("existing");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("kept"), b"existing").unwrap();
    assert!(local::publish(&parent, stage_name, &stage, OsStr::new("existing")).is_err());
    assert_eq!(fs::read(dest.join("kept")).unwrap(), b"existing");
    assert!(stage.names(0).is_err());
    local::discard(&parent, stage_name, stage).unwrap();
    assert!(!fixture.0.join("stage").exists());

    let stage = parent.create_child(stage_name).unwrap();
    stage
        .create_file(OsStr::new("data"))
        .unwrap()
        .write_all(b"complete")
        .unwrap();
    local::publish(&parent, stage_name, &stage, OsStr::new("download")).unwrap();
    assert_eq!(
        fs::read(fixture.0.join("download/data")).unwrap(),
        b"complete"
    );
    assert!(!fixture.0.join("stage").exists());
    let local::Entry::Directory(download) = local::entry(&parent, OsStr::new("download")).unwrap()
    else {
        panic!("published directory could not be opened by its parent handle");
    };
    assert!(
        download
            .same_as(&local::Directory::open_path(&fixture.0.join("download")).unwrap())
            .unwrap()
    );
    let local::Entry::File(mut file) = local::entry(&download, OsStr::new("data")).unwrap() else {
        panic!("published file could not be opened by its parent handle");
    };
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).unwrap();
    assert_eq!(contents, b"complete");
}

#[cfg(windows)]
#[test]
fn private_windows_stage_cleanup_unlinks_a_file_with_an_open_shared_reader() {
    let fixture = Fixture::new();
    let parent = local::Directory::open_path(&fixture.0).unwrap();
    let stage_name = OsStr::new("stage");
    let stage = parent.create_child(stage_name).unwrap();
    let nested = stage.create_child(OsStr::new("nested")).unwrap();
    nested
        .create_file(OsStr::new("data"))
        .unwrap()
        .write_all(b"private bytes")
        .unwrap();
    let mut reader = nested.read_child(OsStr::new("data")).unwrap().unwrap();
    drop(nested);

    local::discard(&parent, stage_name, stage).unwrap();
    assert!(parent.names(1).unwrap().is_empty());
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"private bytes");
}

#[cfg(unix)]
#[test]
fn local_folder_anchors_writes_and_refuses_a_link_in_the_parent_chain() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let folder = fixture.0.join("target");
    let moved = fixture.0.join("moved");
    let outside = fixture.0.join("outside");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(&outside).unwrap();
    let handle = local::Directory::open_path(&folder).unwrap();
    fs::rename(&folder, &moved).unwrap();
    symlink(&outside, &folder).unwrap();
    assert!(local::Directory::open_path(&folder).is_err());
    assert!(local::Directory::open_path(&folder.join("deeper")).is_err());
    handle
        .create_file(OsStr::new("pinned"))
        .unwrap()
        .write_all(b"data")
        .unwrap();
    assert!(!outside.join("pinned").exists());
    assert_eq!(fs::read(moved.join("pinned")).unwrap(), b"data");
}

#[cfg(unix)]
#[test]
#[ignore = "requires an installed OpenSSH sftp-server"]
fn openssh_folder_roundtrip_preserves_collisions_and_cleans_cancelled_downloads() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::process::{Child, Command, Stdio};

    struct Server(Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    let server_path = std::env::var_os("SFTP_SERVER")
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
        })
        .expect("set SFTP_SERVER to the OpenSSH sftp-server executable");
    let mut server = Server(
        Command::new(server_path)
            .arg("-d")
            .arg(&fixture.0)
            .args(["-u", "022"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut client = Sftp::new(
        server.0.stdout.take().unwrap(),
        server.0.stdin.take().unwrap(),
    )
    .unwrap();

    let upload = fixture.0.join("upload");
    fs::create_dir_all(upload.join("nested/empty")).unwrap();
    let payload = vec![0xab; 132_219];
    fs::write(upload.join("nested/asset"), &payload).unwrap();
    fs::write(upload.join("readme.txt"), b"root file").unwrap();
    fs::write(fixture.0.join("outside"), b"private").unwrap();
    symlink(fixture.0.join("outside"), upload.join("escape")).unwrap();
    symlink(&upload, upload.join("loop")).unwrap();
    let remote_path = fixture.0.join("remote");
    let remote = Location::Remote(remote_path.as_os_str().as_bytes().to_vec());
    let upload = Location::Local(upload);
    let summary = copy(&mut client, &upload, &remote, &Progress::new(None), || {
        false
    })
    .unwrap();
    assert!(summary.contains("3 folders · 2 files"), "{summary}");
    assert!(summary.contains("2 links or special entries skipped"));
    assert_eq!(fs::read(remote_path.join("nested/asset")).unwrap(), payload);
    assert!(remote_path.join("nested/empty").is_dir());
    assert!(!remote_path.join("escape").exists());
    assert!(!remote_path.join("loop").exists());
    assert_eq!(
        fs::metadata(&remote_path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    fs::write(remote_path.join("kept"), b"existing").unwrap();
    assert!(
        copy(&mut client, &upload, &remote, &Progress::new(None), || {
            false
        })
        .is_err()
    );
    assert_eq!(fs::read(remote_path.join("kept")).unwrap(), b"existing");

    symlink(fixture.0.join("outside"), remote_path.join("remote-link")).unwrap();
    let download_path = fixture.0.join("download");
    let download = Location::Local(download_path.clone());
    let summary = copy(
        &mut client,
        &remote,
        &download,
        &Progress::new(None),
        || false,
    )
    .unwrap();
    assert!(summary.contains("3 folders · 3 files"));
    assert!(summary.contains("1 links or special entries skipped"));
    assert_eq!(
        fs::read(download_path.join("nested/asset")).unwrap(),
        payload
    );
    assert!(!download_path.join("remote-link").exists());

    let cancel_path = fixture.0.join("cancel");
    let progress = Progress::new(None);
    let result = copy(
        &mut client,
        &remote,
        &Location::Local(cancel_path.clone()),
        &progress,
        || progress.label() != "0 bytes",
    );
    assert!(result.unwrap_err().message.contains("cancelled"));
    assert!(!cancel_path.exists());
    assert!(fs::read_dir(&fixture.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".kortty-part")
    }));
    assert!(
        client
            .read_dir_limited(remote_path.as_os_str().as_bytes(), 3, 1024 * 1024)
            .is_err()
    );
    assert!(client.read_dir(remote_path.as_os_str().as_bytes()).is_ok());
    drop(client);
    assert!(server.0.wait().unwrap().success());
}
