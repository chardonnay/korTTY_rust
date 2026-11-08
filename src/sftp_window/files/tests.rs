use std::io::Write as _;

use super::*;

#[test]
fn remote_names_must_be_a_single_local_component() {
    for name in [
        b"".as_slice(),
        b".",
        b"..",
        b"../outside",
        b"dir/file",
        b"/absolute",
        b"a\0b",
    ] {
        assert_eq!(
            local_name(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(local_name(b"archive.tar.gz").unwrap(), "archive.tar.gz");
    assert_eq!(numbered(b"archive.tar.gz", 2), b"archive (2).tar.gz");
    assert_eq!(numbered(b".profile", 1), b".profile (1)");
}

#[test]
fn transfer_names_cannot_be_reinterpreted_by_a_windows_sftp_peer() {
    for name in [
        br"..\escape".as_slice(),
        b"file:stream",
        b"NUL.txt",
        b"name.",
        b"name ",
    ] {
        assert!(validate_transfer_name(name).is_err());
    }
    assert!(validate_transfer_name(b"normal filename.tar.gz").is_ok());
    assert!(validate_transfer_name(b"raw-unix-\xff").is_ok());
}

#[cfg(windows)]
#[test]
fn windows_rejects_device_names_streams_and_ambiguous_components() {
    for name in [
        "C:\\outside",
        "dir\\file",
        "file:stream",
        "NUL.txt",
        "lpt².log",
        "COM9",
        "end.",
        "end ",
    ] {
        assert_eq!(
            local_name(name.as_bytes()).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(unix)]
#[test]
fn unix_names_round_trip_raw_bytes_and_safe_open_rejects_a_link() {
    let raw = b"file-\xff";
    let name = local_name(raw).unwrap();
    assert_eq!(remote_name(&name).unwrap(), raw);
    let dir = TestDirectory::new();
    let path = dir.0.join("regular-file");
    fs::write(&path, b"private data").unwrap();
    let link = dir.0.join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(
        open_without_links(&path).unwrap().metadata().unwrap().len(),
        12
    );
    assert!(open_without_links(&link).is_err());
    assert!(set_permissions(&link, 0o777).is_err());
}

#[test]
fn stage_drops_partial_output_and_never_clobbers_a_new_destination() {
    let dir = TestDirectory::new();
    let target = dir.0.join("target");
    {
        let mut partial = StagedFile::create(&dir.0).unwrap();
        partial.writer().write_all(b"partial").unwrap();
    }
    assert!(fs::read_dir(&dir.0).unwrap().next().is_none());

    let mut stage = StagedFile::create(&dir.0).unwrap();
    let stage_path = stage.path.clone().unwrap();
    stage.writer().write_all(b"completed transfer").unwrap();
    fs::write(&target, b"another file arrived").unwrap();
    assert_eq!(
        stage.publish(&target, None).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(target).unwrap(), b"another file arrived");
    assert!(!stage_path.exists());
}

#[test]
fn completed_stage_is_published_and_the_temporary_name_is_removed() {
    let dir = TestDirectory::new();
    let target = dir.0.join("target");
    let mut stage = StagedFile::create(&dir.0).unwrap();
    let stage_path = stage.path.clone().unwrap();
    stage.writer().write_all(b"completed transfer").unwrap();
    stage.publish(&target, None).unwrap();
    assert_eq!(fs::read(target).unwrap(), b"completed transfer");
    assert!(!stage_path.exists());
}

#[test]
fn a_replaced_staging_path_is_neither_published_nor_deleted() {
    let dir = TestDirectory::new();
    let target = dir.0.join("target");
    let mut stage = StagedFile::create(&dir.0).unwrap();
    let stage_path = stage.path.clone().unwrap();
    stage.writer().write_all(b"completed transfer").unwrap();
    fs::rename(&stage_path, dir.0.join("displaced")).unwrap();
    fs::write(&stage_path, b"unrelated file").unwrap();

    assert!(stage.publish(&target, None).is_err());
    assert!(!target.exists());
    assert_eq!(fs::read(stage_path).unwrap(), b"unrelated file");
}

#[cfg(unix)]
#[test]
fn overwrite_preserves_mode_and_rejects_a_changed_destination() {
    let dir = TestDirectory::new();
    let target = dir.0.join("target");
    fs::write(&target, b"original").unwrap();
    set_permissions(&target, 0o640).unwrap();
    let expected = local_existing(&target).unwrap().unwrap();

    let mut stage = StagedFile::create(&dir.0).unwrap();
    let stage_path = stage.path.clone().unwrap();
    assert_eq!(
        local_mode(&fs::metadata(&stage_path).unwrap()).unwrap() & 0o077,
        0
    );
    stage.writer().write_all(b"replacement").unwrap();
    stage.publish(&target, Some(&expected)).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(local_mode(&fs::metadata(&target).unwrap()), Some(0o640));
    assert!(!stage_path.exists());

    let expected = local_existing(&target).unwrap().unwrap();
    let mut stage = StagedFile::create(&dir.0).unwrap();
    stage.writer().write_all(b"later transfer").unwrap();
    fs::write(&target, b"changed after approval").unwrap();
    assert!(stage.publish(&target, Some(&expected)).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"changed after approval");
}

#[test]
fn rename_rejects_existing_files_and_moves_a_directory() {
    let dir = TestDirectory::new();
    let source = dir.0.join("source");
    let target = dir.0.join("target");
    fs::write(&source, b"source").unwrap();
    fs::write(&target, b"destination").unwrap();
    assert_eq!(
        rename_local(&source, &target).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&source).unwrap(), b"source");
    assert_eq!(fs::read(&target).unwrap(), b"destination");

    let folder = dir.0.join("folder");
    let moved = dir.0.join("moved");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("child"), b"child").unwrap();
    rename_local(&folder, &moved).unwrap();
    assert!(!folder.exists());
    assert_eq!(fs::read(moved.join("child")).unwrap(), b"child");
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(nonce().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        }
        #[cfg(windows)]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
