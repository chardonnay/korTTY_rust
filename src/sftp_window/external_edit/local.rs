use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kortty_ssh::sftp::MAX_EDIT_BYTES;
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, FileStamp, executable, nonce};

const CONFIG: &str = "sftp-external-editor.json";
const MAX_CONFIG: u64 = 32 * 1024;
const MAX_ARGUMENTS: usize = 24;
const MAX_ARGUMENT_BYTES: usize = 8192;
const TEXT_FILE: &str = "document.txt";
const BINARY_FILE: &str = "document.bin";

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub(super) path: PathBuf,
    pub(super) arguments: Vec<String>,
    pub(super) fingerprint: [u8; 32],
}

pub(super) fn valid_arguments(arguments: &[String]) -> bool {
    arguments.len() <= MAX_ARGUMENTS
        && arguments.iter().map(String::len).sum::<usize>() <= MAX_ARGUMENT_BYTES
        && arguments
            .iter()
            .all(|argument| !argument.chars().any(char::is_control))
}

impl Program {
    pub(super) fn load(root: &Path) -> io::Result<Option<Self>> {
        let root = Directory::open(root, false)?;
        let file = match root.open_private_file(CONFIG) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut json = Vec::new();
        file.take(MAX_CONFIG + 1).read_to_end(&mut json)?;
        if json.len() as u64 > MAX_CONFIG {
            return Err(invalid("External editor approval is too large"));
        }
        let program: Self = serde_json::from_slice(&json).map_err(io::Error::other)?;
        program.validate()?;
        Ok(Some(program))
    }

    pub(super) fn save(&self, root: &Path) -> io::Result<()> {
        self.validate()?;
        let json = serde_json::to_vec(self).map_err(io::Error::other)?;
        if json.len() as u64 > MAX_CONFIG {
            return Err(invalid("External editor approval is too large"));
        }
        let root = Directory::open(root, true)?;
        root.replace_file(CONFIG, &json)?;
        root.sync()
    }

    pub(super) fn review(path: PathBuf, arguments: Vec<String>) -> io::Result<Self> {
        if !executable::valid_path(&path) || !valid_arguments(&arguments) {
            return Err(invalid(
                "Enter an absolute native executable path and bounded arguments",
            ));
        }
        let path = path.canonicalize()?;
        let mut opened = executable::open(&path)?;
        require_native(&mut opened.file)?;
        let fingerprint = executable::digest(&mut opened.file, None)?;
        opened.recheck(&path)?;
        Ok(Self {
            path,
            arguments,
            fingerprint,
        })
    }

    fn validate(&self) -> io::Result<()> {
        if !executable::valid_path(&self.path) || !valid_arguments(&self.arguments) {
            return Err(invalid("The external editor approval is invalid"));
        }
        Ok(())
    }

    pub(super) fn launch(&self, stage: &Stage, stopped: &AtomicBool) -> io::Result<Child> {
        self.validate()?;
        let mut opened = executable::open(&self.path)?;
        require_native(&mut opened.file)?;
        if executable::digest(&mut opened.file, Some(stopped))? != self.fingerprint {
            return Err(invalid(
                "The editor executable changed. Review and approve it again",
            ));
        }
        let path = stage.path();
        let argument = path
            .to_str()
            .ok_or_else(|| invalid("The private local edit path is not UTF-8"))?;
        let arguments = substitute(&self.arguments, argument);
        stage.directory()?;
        opened.recheck(&self.path)?;
        if stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "External edit stopped",
            ));
        }
        Command::new(&self.path)
            .args(arguments)
            .current_dir(&stage.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
}

fn substitute(arguments: &[String], file: &str) -> Vec<String> {
    let mut result: Vec<_> = arguments
        .iter()
        .map(|argument| argument.replace("{file}", file))
        .collect();
    if !arguments.iter().any(|argument| argument.contains("{file}")) {
        result.push(file.to_owned());
    }
    result
}

fn require_native(file: &mut File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata()?.mode() & 0o6000 != 0 {
            return Err(invalid(
                "Select an editor executable without set-ID permissions",
            ));
        }
    }
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes)?;
    #[cfg(windows)]
    let supported = bytes.starts_with(b"MZ");
    #[cfg(target_os = "linux")]
    let supported = &bytes == b"\x7fELF";
    #[cfg(target_os = "macos")]
    let supported = matches!(
        u32::from_be_bytes(bytes),
        0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe | 0xcafe_babf
    );
    if !supported {
        return Err(invalid("Select a native editor executable"));
    }
    Ok(())
}

pub struct Stage {
    root: Directory,
    name: String,
    directory: Directory,
    path: PathBuf,
    filename: &'static str,
    text_only: bool,
}

impl fmt::Debug for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalEditStage")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub(super) bytes: Arc<[u8]>,
    stamp: FileStamp,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalEditSnapshot")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl Stage {
    pub(super) fn create(bytes: &[u8], text_only: bool) -> io::Result<Self> {
        Self::create_at(&std::env::temp_dir(), bytes, text_only)
    }

    fn create_at(root: &Path, bytes: &[u8], text_only: bool) -> io::Result<Self> {
        validate_bytes(bytes, text_only)?;
        let root_path = root.canonicalize()?;
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            if !matches!(root_path.components().next(), Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            {
                return Err(invalid(
                    "External editing needs a private temporary directory on a local disk",
                ));
            }
        }
        #[cfg(unix)]
        let root = Directory::open_owner_controlled(&root_path)?;
        #[cfg(windows)]
        let root = Directory::open(&root_path, false)?;
        let name = format!("kortty-edit-{}", nonce()?);
        let directory = root.create_private_subdirectory(OsStr::new(&name))?;
        let path = root_path.join(&name);
        let stage = Self {
            root,
            name,
            directory,
            path,
            filename: if is_text(bytes) {
                TEXT_FILE
            } else {
                BINARY_FILE
            },
            text_only,
        };
        if let Err(error) = stage.directory.publish_copy(stage.filename, bytes) {
            let _ = stage.remove_directory();
            return Err(error);
        }
        Ok(stage)
    }

    pub(super) fn path(&self) -> PathBuf {
        self.path.join(self.filename)
    }

    fn directory(&self) -> io::Result<Directory> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("The local edit directory has no parent"))?;
        if !self.root.same_as(&Directory::open(parent, false)?)? {
            return Err(invalid("The local edit parent changed"));
        }
        let opened = self.root.private_child(&self.name, false)?;
        if !self.directory.same_as(&opened)? {
            return Err(invalid("The local edit directory changed"));
        }
        Ok(opened)
    }

    pub(super) fn snapshot(&self) -> io::Result<Snapshot> {
        let directory = self.directory()?;
        let mut file = directory.open_private_file(self.filename)?;
        let stamp = FileStamp::read(&file)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_EDIT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        validate_bytes(&bytes, self.text_only)?;
        stamp.require_unchanged(&file)?;
        stamp.require_unchanged(&self.directory()?.open_private_file(self.filename)?)?;
        Ok(Snapshot {
            bytes: bytes.into(),
            stamp,
        })
    }

    pub(super) fn copy_to(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .filter(|_| path.is_absolute())
            .ok_or_else(|| invalid("Enter an absolute path for the local copy"))?;
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .filter(|name| crate::session_journal::export_output::valid_component(name))
            .ok_or_else(|| invalid("Enter an ordinary filename for the local copy"))?;
        if path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(invalid("Enter an absolute path without parent components"));
        }
        let copy = self.snapshot()?;
        Directory::open(parent, false)?.publish_copy(name, &copy.bytes)
    }

    pub(super) fn discard(&self, expected: &Snapshot) -> io::Result<()> {
        let directory = self.directory()?;
        let names = directory.names(2)?;
        if names.len() != 1 || names[0] != self.filename {
            return Err(invalid("The edit directory contains additional files"));
        }
        let current = self.snapshot()?;
        if &current != expected {
            return Err(invalid(
                "The local edit changed. Review the copy before removing it",
            ));
        }
        directory.remove_unchanged(self.filename, &expected.stamp)?;
        self.remove_directory()
    }

    fn remove_directory(&self) -> io::Result<()> {
        self.root
            .remove_deletion_entry(OsStr::new(&self.name), self.directory.identity()?, true)
    }
}

fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

fn validate_bytes(bytes: &[u8], text_only: bool) -> io::Result<()> {
    if bytes.len() > MAX_EDIT_BYTES {
        return Err(invalid("External edit files must be at most 2 MiB"));
    }
    if text_only && !is_text(bytes) {
        return Err(invalid("External edits as root require UTF-8 text"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp_window::external_edit::{Event, Observer, SETTLE};
    use std::time::Instant;

    #[test]
    fn substitution_preserves_each_literal_argument() {
        let template = vec![
            "--file={file}".into(),
            "literal %PATH% $name".into(),
            "".into(),
        ];
        assert_eq!(
            substitute(&template, "C:\\Temp Folder\\document.txt"),
            [
                "--file=C:\\Temp Folder\\document.txt",
                "literal %PATH% $name",
                ""
            ]
        );
        assert_eq!(
            substitute(&["--wait".into()], "/tmp/my edit"),
            ["--wait", "/tmp/my edit"]
        );
        assert!(!valid_arguments(&["x\ny".into()]));
    }

    #[test]
    fn private_copy_observes_atomic_save_and_refuses_stale_cleanup() {
        let first = b"first\x00\xff";
        let later_bytes = b"later\x00\xfe";
        assert!(Stage::create(first, true).is_err());
        let stage = Stage::create(first, false).unwrap();
        assert_eq!(stage.path().extension().unwrap(), "bin");
        let mut observer = Observer::default();
        let opened = stage.snapshot().unwrap();
        assert_eq!(opened.bytes.as_ref(), first);
        let start = Instant::now();
        assert!(observer.observe(Some(opened.clone()), start).is_none());
        assert!(matches!(
            observer.observe(Some(opened.clone()), start + SETTLE),
            Some(Event::Changed(_))
        ));

        stage
            .directory
            .replace_file(BINARY_FILE, later_bytes)
            .unwrap();
        let later = stage.snapshot().unwrap();
        assert_eq!(later.bytes.as_ref(), later_bytes);
        assert!(stage.discard(&opened).is_err());
        let saved = start + SETTLE * 2;
        assert!(observer.observe(Some(later.clone()), saved).is_none());
        assert!(matches!(
            observer.observe(Some(later.clone()), saved + SETTLE),
            Some(Event::Changed(snapshot)) if snapshot.bytes.as_ref() == later_bytes
        ));
        assert!(
            observer
                .observe(Some(later.clone()), saved + SETTLE * 2)
                .is_none()
        );
        stage.discard(&later).unwrap();
        assert!(!stage.path().exists());
    }

    #[test]
    fn save_local_copy_does_not_replace_an_existing_file() {
        let source = Stage::create(b"edited\x00\xff", false).unwrap();
        let target = Stage::create(b"existing", true).unwrap();
        assert!(source.copy_to(&target.path()).is_err());
        assert_eq!(target.snapshot().unwrap().bytes.as_ref(), b"existing");
        let copy = target.path.join("saved.bin");
        source.copy_to(&copy).unwrap();
        assert_eq!(std::fs::read(copy).unwrap(), b"edited\x00\xff");
        target.directory.remove_copy("saved.bin").unwrap();
        target.discard(&target.snapshot().unwrap()).unwrap();
        source.discard(&source.snapshot().unwrap()).unwrap();
    }
}
