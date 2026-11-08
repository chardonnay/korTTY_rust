use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct TempDirectory(pub(super) PathBuf);

impl TempDirectory {
    pub(super) fn new(fixture: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        let root = std::env::temp_dir();
        let process = std::process::id();
        for _ in 0..256 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("kortty-{fixture}-{process}-{sequence}"));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                // A previous process with this PID may have left a fixture behind.
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("could not create fixture directory {path:?}: {error}"),
            }
        }
        panic!("could not allocate a unique {fixture} fixture directory under {root:?}");
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
