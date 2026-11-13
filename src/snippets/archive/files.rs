//! Native snippet folder and ZIP export, sharing one set of checked relative paths.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Cursor, Seek, Write};
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, System, ZipWriter};

#[cfg(windows)]
use kortty_files::tree::Directory;

use super::{FolderSelection, Snippet, absolute, invalid, profile};
use crate::snippets::recipients::Choice;

mod layout;
mod openpgp;
pub(crate) use layout::{AnalysisFile, analysis_files};
pub(crate) use openpgp::{
    CertificateInfo, MAX_CERTIFICATE_BYTES, encrypt_profile_zip, encrypt_saved_payload,
    enroll_certificate, inspect_certificate, normalize_fingerprint, validate_backup_certificate,
};

#[derive(Clone, Copy)]
pub(super) enum Source<'a> {
    Snippets(&'a [Snippet]),
    Folder(&'a FolderSelection),
}

impl<'a> Source<'a> {
    fn entries(self) -> Vec<(&'a Snippet, &'a str)> {
        match self {
            Self::Snippets(snippets) => snippets
                .iter()
                .map(|snippet| (snippet, snippet.metadata().folder_path.as_str()))
                .collect(),
            Self::Folder(folder) => folder
                .entries
                .iter()
                .map(|entry| (&entry.snippet, entry.directory.as_str()))
                .collect(),
        }
    }

    fn directories(self) -> &'a [String] {
        match self {
            Self::Snippets(_) => &[],
            Self::Folder(folder) => &folder.directories,
        }
    }

    fn extension(self, extension: Option<&str>) -> Option<&str> {
        match self {
            Self::Snippets(_) => extension,
            Self::Folder(_) => None,
        }
    }
}

#[derive(Eq, PartialEq)]
pub(crate) struct ZipSettings {
    extension: Option<String>,
    password: Option<Zeroizing<String>>,
}

impl ZipSettings {
    pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;

    pub(crate) fn new(extension: &str, password: Option<Zeroizing<String>>) -> io::Result<Self> {
        let extension = layout::forced_extension(extension)?;
        if password
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > Self::MAX_PASSWORD_BYTES)
        {
            return Err(invalid("A ZIP password must contain 1 to 1024 UTF-8 bytes"));
        }
        Ok(Self {
            extension,
            password,
        })
    }
}

#[derive(Eq, PartialEq)]
pub(crate) struct OpenPgpSettings {
    extension: Option<String>,
    recipient: openpgp::Recipient,
}

impl OpenPgpSettings {
    pub(crate) fn new(
        extension: &str,
        certificate: PathBuf,
        fingerprint: &str,
    ) -> io::Result<Self> {
        Ok(Self {
            extension: layout::forced_extension(extension)?,
            recipient: openpgp::Recipient::new(certificate, fingerprint)?,
        })
    }

    pub(crate) fn saved(extension: &str, path: PathBuf, choice: Choice) -> io::Result<Self> {
        Ok(Self {
            extension: layout::forced_extension(extension)?,
            recipient: openpgp::Recipient::saved(path, choice)?,
        })
    }
}

pub(super) fn directory(target: &Path, source: Source<'_>) -> io::Result<()> {
    absolute(target)?;
    let parent = target
        .parent()
        .filter(|_| target.file_name().is_some())
        .ok_or_else(|| invalid("Enter a new directory path"))?;
    match target.symlink_metadata() {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "The export directory already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let layout = layout::Layout::build(source, None)?;
    let stage = StagingDirectory::new(parent)?;
    for dir in &layout.directories {
        let path = stage.path.join(dir);
        create_directory(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
    }
    for entry in layout.entries {
        let path = stage.path.join(entry.path);
        let mut file = create_file(&path)?;
        file.write_all(file_content(entry.snippet, true).as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(entry.mode))?;
        }
        file.sync_all()?;
    }
    stage.publish(target)
}

pub(super) fn zip(target: &Path, source: Source<'_>, settings: ZipSettings) -> io::Result<()> {
    absolute(target)?;
    let target = with_extension(target, "zip", ".zip")?;
    let extension = source.extension(settings.extension.as_deref());
    let layout = layout::Layout::build(source, extension)?;
    profile::create_new_atomic(&target, |file| {
        write_zip(
            file,
            layout,
            settings.password.as_deref().map(String::as_str),
            extension.is_none(),
        )
    })
}

pub(super) fn encrypted_zip(
    target: &Path,
    source: Source<'_>,
    settings: OpenPgpSettings,
) -> io::Result<()> {
    absolute(target)?;
    let target = with_extension(target, "gpg", ".zip.gpg")?;
    let certificate = settings.recipient.load()?;
    let encryption = openpgp::Encryption::select(&certificate)?;
    let extension = source.extension(settings.extension.as_deref());
    let layout = layout::Layout::build(source, extension)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    write_zip(
        Cursor::new(&mut *plaintext),
        layout,
        None,
        extension.is_none(),
    )?;
    profile::create_new_atomic(&target, |file| encryption.write(&plaintext, file))
}

fn with_extension(target: &Path, extension: &str, suffix: &str) -> io::Result<PathBuf> {
    let mut name = target
        .file_name()
        .ok_or_else(|| invalid("Enter a ZIP file path"))?
        .to_owned();
    if !target
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    {
        name.push(suffix);
    }
    Ok(target.with_file_name(name))
}

fn write_zip(
    output: impl Write + Seek,
    layout: layout::Layout<'_>,
    password: Option<&str>,
    normalize: bool,
) -> io::Result<()> {
    let mut archive = ZipWriter::new(output);
    let options = SimpleFileOptions::default().system(System::Unix);
    for directory in layout.directories {
        archive.add_directory(directory, options.unix_permissions(0o755))?;
    }
    for entry in layout.entries {
        let mut options = options
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(entry.mode);
        if let Some(password) = password {
            options = options.with_aes_encryption(AesMode::Aes256, password);
        }
        archive.start_file(entry.path, options)?;
        archive.write_all(file_content(entry.snippet, normalize).as_bytes())?;
    }
    archive.finish()?;
    Ok(())
}

fn file_content(snippet: &Snippet, normalize: bool) -> Cow<'_, str> {
    let content = snippet.content();
    if !normalize {
        return Cow::Borrowed(content);
    }
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if content.contains('\r') {
        Cow::Owned(content.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(content)
    }
}

#[cfg(unix)]
fn create_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(unix)]
fn create_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(windows)]
fn create_directory(path: &Path) -> io::Result<()> {
    let (parent, name) = windows_parent(path)?;
    parent.create_child(name)?;
    Ok(())
}

#[cfg(windows)]
fn create_file(path: &Path) -> io::Result<File> {
    let (parent, name) = windows_parent(path)?;
    parent.create_file(name)
}

#[cfg(windows)]
fn windows_parent(path: &Path) -> io::Result<(Directory, &std::ffi::OsStr)> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(invalid("Expected an absolute snippet export path"));
    };
    Ok((Directory::open_path(parent)?, name))
}

struct StagingDirectory {
    path: PathBuf,
    published: bool,
    #[cfg(windows)]
    handles: Option<(Directory, Directory)>,
}

impl StagingDirectory {
    fn new(parent: &Path) -> io::Result<Self> {
        for _ in 0..16 {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(io::Error::other)?;
            let name = format!(".kortty-export-{:032x}", u128::from_ne_bytes(nonce));
            match Self::create(parent.join(name)) {
                Ok(stage) => return Ok(stage),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "Could not create the export staging directory",
        ))
    }

    fn create(path: PathBuf) -> io::Result<Self> {
        #[cfg(unix)]
        create_directory(&path)?;
        #[cfg(windows)]
        let handles = {
            let (parent, name) = windows_parent(&path)?;
            let stage = parent.create_child(name)?;
            Some((parent, stage))
        };
        Ok(Self {
            path,
            published: false,
            #[cfg(windows)]
            handles,
        })
    }

    fn publish(mut self, target: &Path) -> io::Result<()> {
        self.rename(target).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Could not publish the folder export: {error}"),
            )
        })?;
        self.published = true;
        Ok(())
    }

    #[cfg(unix)]
    fn rename(&self, target: &Path) -> io::Result<()> {
        profile::publish_new(&self.path, target)
    }

    #[cfg(windows)]
    fn rename(&self, target: &Path) -> io::Result<()> {
        let name = target
            .file_name()
            .filter(|_| target.parent() == self.path.parent())
            .ok_or_else(|| invalid("The export must be published next to its staging directory"))?;
        let (parent, stage) = self.handles.as_ref().expect("staging handles are live");
        parent.publish(stage, name)
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            #[cfg(windows)]
            drop(self.handles.take());
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(all(test, windows))]
#[test]
fn folder_export_retries_publication_while_a_descendant_is_open() {
    use std::ffi::OsStr;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::time::Duration;

    let profile = crate::snippets::tests::Profile::new();
    let stage = StagingDirectory::new(&profile.0).unwrap();
    let file = stage.path.join("fixture.txt");
    let mut output = create_file(&file).unwrap();
    output.write_all(b"disposable fixture").unwrap();
    drop(output);
    let blocker = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    let (parent, directory) = stage.handles.as_ref().unwrap();
    let name = OsStr::new("published");
    let error = parent.publish(directory, name).unwrap_err();
    assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
    let target = profile.0.join(name);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            drop(blocker);
        });
        stage.publish(&target).unwrap();
    });
    assert_eq!(
        fs::read(target.join("fixture.txt")).unwrap(),
        b"disposable fixture"
    );
}
