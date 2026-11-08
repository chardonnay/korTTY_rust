//! Publishes journal exports only after the destination has been confirmed.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Cursor, Seek, Write};
use std::path::PathBuf;

use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, System, ZipWriter};

use crate::private_files::{Directory, EntryIdentity, nonce};
use crate::snippets::{
    archive::{ZipSettings, encrypt_saved_payload},
    recipients::Choice,
};

use super::export::{Markdown, Options, Selection, Snapshot, TopicSelection};
use super::{MAX_CATALOG_ROWS, check_path};

const MAX_BATCH_BYTES: usize = 128 * 1024 * 1024;
const MAX_BATCH_FILES: usize = 4096;
const MAX_STEM_BYTES: usize = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Format {
    Pdf,
    Markdown,
    HtmlBundle,
}

impl Format {
    pub(crate) const ALL: [Self; 3] = [Self::Pdf, Self::Markdown, Self::HtmlBundle];

    pub(crate) fn offers_screenshots(self) -> bool {
        self != Self::HtmlBundle
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pdf => "PDF",
            Self::Markdown => "Markdown",
            Self::HtmlBundle => "HTML bundle (complete)",
        })
    }
}

#[derive(Clone)]
pub(crate) enum Protection {
    None,
    Password(Zeroizing<String>),
    OpenPgp { profile: PathBuf, recipient: Choice },
}

impl fmt::Debug for Protection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "None",
            Self::Password(_) => "Password([redacted])",
            Self::OpenPgp { .. } => "OpenPgp",
        })
    }
}

impl Protection {
    pub(crate) fn extension(&self, format: Format, multiple: bool) -> &'static str {
        match self {
            Self::None if format == Format::Pdf && !multiple => ".pdf",
            Self::None if format == Format::Markdown && !multiple => ".md",
            Self::None | Self::Password(_) => ".zip",
            Self::OpenPgp { .. } if format == Format::Pdf && !multiple => ".pdf.gpg",
            Self::OpenPgp { .. } => ".zip.gpg",
        }
    }

    fn validate(&self) -> io::Result<()> {
        if let Self::Password(password) = self
            && (password.is_empty() || password.len() > ZipSettings::MAX_PASSWORD_BYTES)
        {
            return Err(invalid("A ZIP password must contain 1 to 1024 UTF-8 bytes"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Destination {
    path: PathBuf,
    parent_identity: EntryIdentity,
}

mod screenshot;
pub(crate) use screenshot::{PngDestination, PublishedScreenshot};

impl Destination {
    /// Binds the confirmed filename to the directory observed by the save browser.
    pub(crate) fn new(path: PathBuf, parent_identity: EntryIdentity) -> io::Result<Self> {
        check_path(&path)?;
        if path.parent().is_none()
            || path
                .file_name()
                .and_then(OsStr::to_str)
                .is_none_or(|name| !valid_component(name))
        {
            return Err(invalid("Enter a safe session journal export filename"));
        }
        Ok(Self {
            path,
            parent_identity,
        })
    }

    fn open_parent(&self) -> io::Result<Directory> {
        let directory = Directory::open(self.path.parent().expect("validated parent"), false)?;
        if directory.identity()? != self.parent_identity {
            return Err(invalid(
                "The selected export folder changed. Select it again.",
            ));
        }
        Ok(directory)
    }

    fn require_current(&self, parent: &Directory, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        check_cancelled(cancelled)?;
        if !parent.same_as(&self.open_parent()?)? {
            return Err(invalid(
                "The selected export folder changed. Select it again.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub(crate) struct Published {
    pub path: PathBuf,
    pub exported: usize,
    pub skipped: Vec<String>,
    pub skipped_assets: usize,
    pub incomplete_capture_journals: usize,
    pub topic_notices: Vec<String>,
    pub warning: Option<String>,
}

type JournalInput = (String, io::Result<Snapshot>);

pub(crate) struct Request<'a> {
    pub format: Format,
    pub options: &'a Options,
    pub protection: &'a Protection,
    pub links_allowed: bool,
    pub topic_ai: Option<&'a TopicSelection>,
}

/// Reads and renders one selected journal at a time. This is the write boundary for Save.
pub(crate) fn publish(
    destination: &Destination,
    selected_count: usize,
    journals: impl IntoIterator<Item = JournalInput>,
    request: Request<'_>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Published> {
    if !(1..=MAX_CATALOG_ROWS).contains(&selected_count) {
        return Err(invalid("Select between 1 and 512 session journals"));
    }
    let Request {
        format,
        options,
        protection,
        links_allowed,
        topic_ai,
    } = request;
    protection.validate()?;
    check_cancelled(cancelled)?;
    let name = destination
        .path
        .file_name()
        .and_then(OsStr::to_str)
        .expect("validated filename");
    let extension = protection.extension(format, selected_count > 1);
    let stem = name
        .get(..name.len().saturating_sub(extension.len()))
        .filter(|stem| valid_component(stem) && name[stem.len()..].eq_ignore_ascii_case(extension))
        .ok_or_else(|| invalid("The export filename does not match the selected format"))?;
    let parent = destination.open_parent()?;
    let batch = Batch {
        selected_count,
        stem,
        format,
        options,
        links_allowed,
        topic_ai,
        cancelled,
    };
    let mut published = Published {
        path: destination.path.clone(),
        ..Published::default()
    };
    let check = || destination.require_current(&parent, cancelled);
    match protection {
        Protection::None if format == Format::Markdown && selected_count == 1 => {
            let snapshot = single_journal(journals)?;
            let folder = asset_folder(stem)?;
            let selection = batch.select(&snapshot)?;
            let markdown = selection.render(options, &folder)?;
            if let Some(notice) = selection.notice {
                published.topic_notices.push(notice.message());
            }
            Budget::default().include(&markdown)?;
            check()?;
            publish_markdown(&parent, name, &folder, &markdown, check, cancelled)?;
            published.exported = 1;
            published.skipped_assets = markdown.skipped_assets;
        }
        Protection::None | Protection::OpenPgp { .. }
            if format == Format::Pdf && selected_count == 1 =>
        {
            let snapshot = single_journal(journals)?;
            let selection = batch.select(&snapshot)?;
            let pdf = selection.render_pdf(options, &options.branding, cancelled)?;
            if let Some(notice) = selection.notice {
                published.topic_notices.push(notice.message());
            }
            Budget::default().include_file(pdf.bytes.len())?;
            check()?;
            let bytes = Zeroizing::new(pdf.bytes);
            parent.publish_checked(
                name,
                |file| {
                    if let Protection::OpenPgp { profile, recipient } = protection {
                        let result = encrypt_saved_payload(
                            profile,
                            recipient,
                            &bytes,
                            CancelWriter { file, cancelled },
                            &format!("{stem}.pdf"),
                        );
                        check_cancelled(cancelled)?;
                        result
                    } else {
                        write_bytes(file, &bytes, cancelled)
                    }
                },
                check,
            )?;
            published.exported = 1;
            published.skipped_assets = pdf.skipped_assets;
        }
        Protection::OpenPgp { profile, recipient } => {
            // The bounded unencrypted archive never needs a plaintext file.
            let mut plaintext = Zeroizing::new(Vec::new());
            batch.write_zip(Cursor::new(&mut *plaintext), journals, None, &mut published)?;
            check()?;
            parent.publish_checked(
                name,
                |file| {
                    let result = encrypt_saved_payload(
                        profile,
                        recipient,
                        &plaintext,
                        CancelWriter { file, cancelled },
                        "kortty-journals.zip",
                    );
                    check_cancelled(cancelled)?;
                    result
                },
                check,
            )?;
        }
        Protection::None | Protection::Password(_) => {
            let password = match protection {
                Protection::Password(password) => Some(password.as_str()),
                _ => None,
            };
            parent.publish_checked(
                name,
                |file| batch.write_zip(file, journals, password, &mut published),
                check,
            )?;
        }
    }
    published.warning = parent.sync().err().map(|error| {
        format!("The export was saved. Its folder could not be synchronized ({error}).")
    });
    Ok(published)
}

fn single_journal(journals: impl IntoIterator<Item = JournalInput>) -> io::Result<Snapshot> {
    let mut journals = journals.into_iter();
    let (_, snapshot) = journals.next().ok_or_else(selection_changed)?;
    let snapshot = snapshot?;
    if journals.next().is_some() {
        return Err(selection_changed());
    }
    Ok(snapshot)
}

struct Batch<'a> {
    selected_count: usize,
    stem: &'a str,
    format: Format,
    options: &'a Options,
    links_allowed: bool,
    topic_ai: Option<&'a TopicSelection>,
    cancelled: &'a dyn Fn() -> bool,
}

impl Batch<'_> {
    fn select<'a>(&self, snapshot: &'a Snapshot) -> io::Result<Selection<'a>> {
        snapshot.select_for_export(&self.options.filter, self.topic_ai, self.cancelled)
    }

    fn write_zip(
        &self,
        output: impl Write + Seek,
        journals: impl IntoIterator<Item = JournalInput>,
        password: Option<&str>,
        published: &mut Published,
    ) -> io::Result<()> {
        let mut zip = ZipWriter::new(output);
        let mut budget = Budget::default();
        let mut used = HashSet::new();
        let mut members = HashSet::new();
        let files = SimpleFileOptions::default()
            .system(System::Unix)
            .unix_permissions(0o600)
            .compression_method(CompressionMethod::Deflated);
        let files = match password {
            Some(password) => files.with_aes_encryption(AesMode::Aes256, password),
            None => files,
        };
        let mut write_member = |name: &str, bytes: &[u8]| -> io::Result<()> {
            check_cancelled(self.cancelled)?;
            if !name.split('/').all(valid_component) || !members.insert(name.to_lowercase()) {
                return Err(invalid("Invalid or duplicate journal archive member name"));
            }
            budget.include_file(bytes.len())?;
            zip.start_file(name, files)?;
            write_bytes(&mut zip, bytes, self.cancelled)
        };
        let mut journals = journals.into_iter();
        for _ in 0..self.selected_count {
            check_cancelled(self.cancelled)?;
            let (label, snapshot) = journals.next().ok_or_else(selection_changed)?;
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error)
                    if self.selected_count == 1 || error.kind() == io::ErrorKind::Interrupted =>
                {
                    return Err(error);
                }
                Err(error) => {
                    published.skipped.push(skipped(&label, &error.to_string()));
                    continue;
                }
            };
            let selection = self.select(&snapshot)?;
            if self.options.filter.is_active() && selection.is_empty() {
                let mut reason = "No entries matched the export filters".to_owned();
                if let Some(notice) = selection.notice {
                    reason.push_str(". ");
                    reason.push_str(&notice.message());
                }
                published.skipped.push(skipped(&label, &reason));
                continue;
            }
            if let Some(notice) = selection.notice {
                published
                    .topic_notices
                    .push(skipped(&label, &notice.message()));
            }
            let stem = if self.selected_count == 1 {
                self.stem.to_owned()
            } else {
                let title = snapshot.title();
                let base = if title.trim().is_empty() {
                    snapshot
                        .directory()
                        .file_name()
                        .and_then(OsStr::to_str)
                        .unwrap_or_default()
                } else {
                    title
                };
                unique_stem(&mut used, export_basename(base))
            };
            let skipped_assets = match self.format {
                Format::Pdf => {
                    let pdf = selection.render_pdf(
                        self.options,
                        &self.options.branding,
                        self.cancelled,
                    )?;
                    write_member(&format!("{stem}.pdf"), &pdf.bytes)?;
                    pdf.skipped_assets
                }
                Format::Markdown => {
                    let folder = asset_folder(&stem)?;
                    let markdown = selection.render(self.options, &folder)?;
                    write_member(&format!("{stem}.md"), markdown.text.as_bytes())?;
                    for asset in markdown.assets {
                        if !valid_component(&asset.name) {
                            return Err(invalid("Invalid journal export asset name"));
                        }
                        write_member(&format!("{folder}/{}", asset.name), &asset.bytes)?;
                    }
                    markdown.skipped_assets
                }
                Format::HtmlBundle => {
                    let stats = selection.write_html_bundle(
                        self.options,
                        self.links_allowed,
                        |name, bytes| {
                            if self.selected_count == 1 {
                                write_member(name, bytes)
                            } else {
                                write_member(&format!("{stem}/{name}"), bytes)
                            }
                        },
                        self.cancelled,
                    )?;
                    published.incomplete_capture_journals += usize::from(stats.capture_incomplete);
                    stats.skipped_assets
                }
            };
            published.exported += 1;
            published.skipped_assets += skipped_assets;
        }
        check_cancelled(self.cancelled)?;
        if journals.next().is_some() {
            return Err(selection_changed());
        }
        if published.exported == 0 {
            let reason = published
                .skipped
                .first()
                .map(String::as_str)
                .unwrap_or("No matching journals");
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("No selected session journals could be exported ({reason})"),
            ));
        }
        zip.finish()?;
        Ok(())
    }
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    files: usize,
}

impl Budget {
    fn include(&mut self, markdown: &Markdown) -> io::Result<()> {
        let mut names = HashSet::new();
        self.include_file(markdown.text.len())?;
        for asset in &markdown.assets {
            if !valid_component(&asset.name) || !names.insert(asset.name.to_lowercase()) {
                return Err(invalid("Invalid or duplicate journal export asset name"));
            }
            self.include_file(asset.bytes.len())?;
        }
        Ok(())
    }

    fn include_file(&mut self, size: usize) -> io::Result<()> {
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(size);
        if self.bytes > MAX_BATCH_BYTES || self.files > MAX_BATCH_FILES {
            return Err(invalid(
                "The journal export exceeds the 128 MiB or 4096 file limit",
            ));
        }
        Ok(())
    }
}

fn publish_markdown(
    parent: &Directory,
    name: &str,
    folder: &str,
    markdown: &Markdown,
    check: impl Fn() -> io::Result<()>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    let mut assets = if markdown.assets.is_empty() {
        None
    } else {
        let mut stage = AssetStage::new(parent, folder)?;
        for asset in &markdown.assets {
            check_cancelled(cancelled)?;
            stage.write(&asset.name, &asset.bytes, cancelled)?;
        }
        Some(stage)
    };
    parent.publish_checked(
        name,
        |file| write_bytes(file, markdown.text.as_bytes(), cancelled),
        || {
            check()?;
            if let Some(stage) = &mut assets {
                // The Markdown rename is the commit point. A competing destination
                // rolls back only the assets this export just published.
                stage.publish()?;
                check()?;
            }
            Ok(())
        },
    )?;
    if let Some(stage) = &mut assets {
        stage.assets = None;
    }
    Ok(())
}

struct AssetStage<'a> {
    parent: &'a Directory,
    stage: Directory,
    stage_name: String,
    stage_identity: EntryIdentity,
    asset_name: String,
    assets: Option<(Directory, EntryIdentity)>,
    files: Vec<(String, EntryIdentity)>,
    published: bool,
}

impl<'a> AssetStage<'a> {
    fn new(parent: &'a Directory, asset_name: &str) -> io::Result<Self> {
        let stage_name = format!(".kortty-journal-export-{}", nonce()?);
        let stage = parent.create_private_subdirectory(OsStr::new(&stage_name))?;
        let mut output = Self {
            parent,
            stage_identity: stage.identity()?,
            stage,
            stage_name,
            asset_name: asset_name.into(),
            assets: None,
            files: Vec::new(),
            published: false,
        };
        let assets = output
            .stage
            .create_private_subdirectory(OsStr::new(asset_name))?;
        let identity = assets.identity()?;
        output.assets = Some((assets, identity));
        Ok(output)
    }

    fn write(&mut self, name: &str, bytes: &[u8], cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        let (assets, _) = self.assets.as_ref().expect("active asset stage");
        assets.publish_with(name, |file| write_bytes(file, bytes, cancelled))?;
        let (identity, _) = assets.deletion_entry(OsStr::new(name))?;
        self.files.push((name.into(), identity));
        Ok(())
    }

    fn publish(&mut self) -> io::Result<()> {
        let name = OsStr::new(&self.asset_name);
        let (assets, _) = self.assets.as_ref().expect("active asset stage");
        if !assets.same_as(&self.stage.deletion_child(name)?)? {
            return Err(invalid(
                "The staged journal assets changed before publication",
            ));
        }
        assets.sync()?;
        self.stage.move_entry(name, self.parent)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for AssetStage<'_> {
    fn drop(&mut self) {
        if let Some((assets, identity)) = self.assets.take() {
            let parent = if self.published {
                self.parent
            } else {
                &self.stage
            };
            let name = OsStr::new(&self.asset_name);
            let unchanged = parent
                .deletion_child(name)
                .and_then(|current| current.same_as(&assets))
                .unwrap_or(false);
            if unchanged {
                for (file, identity) in &self.files {
                    let _ = assets.remove_deletion_entry(OsStr::new(file), *identity, false);
                }
                drop(assets);
                let _ = parent.remove_deletion_entry(name, identity, true);
            }
        }
        let _ = self.parent.remove_deletion_entry(
            OsStr::new(&self.stage_name),
            self.stage_identity,
            true,
        );
    }
}

struct CancelWriter<'a, W> {
    file: W,
    cancelled: &'a dyn Fn() -> bool,
}

impl<W: Write> Write for CancelWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            // Write::write_all retries Interrupted; use a terminating I/O error here.
            return Err(io::Error::other("Session journal export cancelled"));
        }
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn write_bytes(
    output: &mut impl Write,
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    for chunk in bytes.chunks(64 * 1024) {
        check_cancelled(cancelled)?;
        output.write_all(chunk)?;
    }
    Ok(())
}

fn check_cancelled(cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Session journal export cancelled",
        ))
    } else {
        Ok(())
    }
}

fn asset_folder(stem: &str) -> io::Result<String> {
    let folder = format!("{stem}-files");
    if valid_component(&folder) {
        Ok(folder)
    } else {
        Err(invalid(
            "The journal export attachment folder name is too long or unsafe",
        ))
    }
}

/// Uses the Java filename character set with portable bounds and reserved-name checks.
pub(crate) fn export_basename(title: &str) -> String {
    let mut name = String::new();
    for character in title.trim().chars() {
        if name.len() == MAX_STEM_BYTES {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-') {
            name.push(character);
        } else if !name.is_empty() && !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches(['_', '.']);
    if valid_component(name) {
        name.to_owned()
    } else {
        "session-journal".into()
    }
}

fn unique_stem(used: &mut HashSet<String>, base: String) -> String {
    if used.insert(base.to_ascii_lowercase()) {
        return base;
    }
    for index in 2.. {
        let name = format!("{base}-{index}");
        if used.insert(name.to_ascii_lowercase()) {
            return name;
        }
    }
    unreachable!("journal archive name uniqueness is bounded by its selection")
}

pub(crate) fn valid_component(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.ends_with([' ', '.'])
        || name.chars().any(|value| {
            !super::is_display_character(value)
                || value.is_control()
                || matches!(value, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn skipped(label: &str, reason: &str) -> String {
    let label: String = label
        .chars()
        .filter(|value| super::is_display_character(*value))
        .take(160)
        .collect();
    let reason: String = reason
        .chars()
        .filter(|value| super::is_display_character(*value))
        .take(220)
        .collect();
    format!("{label}: {reason}")
}

fn selection_changed() -> io::Error {
    invalid("The journal export selection changed. Select the journals again.")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
