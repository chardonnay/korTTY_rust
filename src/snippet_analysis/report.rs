//! Bounded PRE analysis reports from an immutable history snapshot.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use zip::write::SimpleFileOptions;

use crate::private_files::Directory;
use crate::session_journal::export_output::valid_component;

use super::model::{Record, Subject};

mod render;
#[cfg(test)]
mod tests;

pub(crate) const MAX_REPORTS: usize = 256;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FOLDER_BYTES: usize = 1024;
const MAX_FOLDER_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Html,
    Markdown,
    Json,
}

impl Format {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Html => "HTML",
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Packing {
    Single,
    Combined,
    Zip,
}

impl fmt::Display for Packing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Single => "Single report",
            Self::Combined => "Combined report",
            Self::Zip => "ZIP archive",
        })
    }
}

pub(crate) struct Options {
    pub(crate) format: Format,
    pub(crate) packing: Packing,
    pub(crate) include_code: bool,
    pub(crate) exported_at: u64,
    pub(crate) locale: String,
    pub(crate) skipped: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct Item {
    pub(crate) subject: Subject,
    pub(crate) record: Record,
    pub(crate) folder_path: String,
    pub(crate) current_hash: Option<String>,
}

impl Item {
    fn stale(&self) -> bool {
        !self.record.source.sha256.is_empty()
            && self
                .current_hash
                .as_ref()
                .is_some_and(|hash| hash != &self.record.source.sha256)
    }
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) exported: usize,
    pub(crate) skipped: usize,
    pub(crate) failed_diagrams: usize,
    pub(crate) written_files: Vec<PathBuf>,
    pub(crate) warning: Option<String>,
}

pub(crate) fn export(
    target: PathBuf,
    reports: Vec<Item>,
    options: Options,
    cancelled: &AtomicBool,
) -> io::Result<Outcome> {
    check_cancelled(cancelled)?;
    validate_selection(&reports, &options)?;
    let (folder, name) = destination(&target)?;
    let directory = Directory::open(folder, false)?;
    if directory.has_entry(name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The report export destination already exists",
        ));
    }
    let publication = directory.publish_checked(
        name,
        |output| write_output(output, &reports, &options, cancelled),
        || {
            check_cancelled(cancelled)?;
            if !directory.same_as(&Directory::open(folder, false)?)? {
                return Err(invalid("The selected report export folder changed"));
            }
            Ok(())
        },
    );
    if publication.is_err() {
        check_cancelled(cancelled)?;
        publication?;
    }
    Ok(Outcome {
        exported: reports.len(),
        skipped: options.skipped.len(),
        failed_diagrams: 0,
        written_files: vec![target],
        warning: directory.sync().err().map(|error| {
            format!("The report was saved, but its directory could not be synced: {error}")
        }),
    })
}

fn validate_selection(reports: &[Item], options: &Options) -> io::Result<()> {
    if reports.is_empty()
        || reports.len().saturating_add(options.skipped.len()) > MAX_REPORTS
        || (options.packing == Packing::Single
            && (reports.len() != 1 || !options.skipped.is_empty()))
    {
        return Err(invalid("Select one to 256 reports for this export"));
    }
    if options.locale.is_empty()
        || options.locale.len() > 64
        || !options
            .locale
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || timestamp(options.exported_at).is_none()
    {
        return Err(invalid("The report export date or language is invalid"));
    }
    Ok(())
}

fn destination(path: &Path) -> io::Result<(&Path, &str)> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(invalid("Select an absolute report export path"));
    }
    path.parent()
        .zip(path.file_name().and_then(OsStr::to_str))
        .filter(|(_, name)| valid_component(name))
        .ok_or_else(|| invalid("Enter a safe report export filename"))
}

fn write_output(
    output: &mut File,
    reports: &[Item],
    options: &Options,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut remaining = MAX_OUTPUT_BYTES;
    if options.packing != Packing::Zip {
        return render::write(
            &mut CheckedWriter::new(output, &mut remaining, cancelled),
            reports,
            options,
            options.packing,
        );
    }

    let mut archive = zip::ZipWriter::new(output);
    let entry_options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    for (index, item) in reports.iter().enumerate() {
        check_cancelled(cancelled)?;
        archive.start_file(archive_name(index, item, options.format)?, entry_options)?;
        render::write(
            &mut CheckedWriter::new(&mut archive, &mut remaining, cancelled),
            std::slice::from_ref(item),
            options,
            Packing::Zip,
        )?;
    }
    if !options.skipped.is_empty() {
        check_cancelled(cancelled)?;
        archive.start_file("skipped.txt", entry_options)?;
        let mut writer = CheckedWriter::new(&mut archive, &mut remaining, cancelled);
        writeln!(writer, "Snippets without a stored analysis")?;
        for name in &options.skipped {
            writeln!(writer, "- {}", render::single_line(name))?;
        }
    }
    check_cancelled(cancelled)?;
    archive.finish()?;
    Ok(())
}

fn archive_name(index: usize, item: &Item, format: Format) -> io::Result<String> {
    if item.folder_path.len() > MAX_FOLDER_BYTES
        || item.folder_path.split('/').count() > MAX_FOLDER_DEPTH
    {
        return Err(invalid(
            "A snippet report folder path exceeds the export limit",
        ));
    }
    let mut path = String::new();
    if !item.folder_path.is_empty() {
        for part in item.folder_path.split('/') {
            if !valid_component(part) {
                return Err(invalid("A snippet report folder path is unsafe"));
            }
            path.push_str(part);
            path.push('/');
        }
    }
    let mut base = String::new();
    for character in item.record.source.snippet_name.chars() {
        if base.len() + character.len_utf8() > 80 {
            break;
        }
        if character.is_alphanumeric() || matches!(character, '.' | '_' | '-') {
            base.push(character);
        } else if !base.ends_with('_') {
            base.push('_');
        }
    }
    let base = base.trim_matches(['.', '_']);
    let base = if base.is_empty() { "code" } else { base };
    let date = timestamp(item.record.analyzed_at).map_or_else(String::new, |time| {
        format!(
            "-{}",
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y%m%d-%H%M")
        )
    });
    path.push_str(&format!(
        "{:02}-{base}-analysis{date}.{}",
        index + 1,
        format.extension()
    ));
    Ok(path)
}

fn timestamp(millis: u64) -> Option<Timestamp> {
    Timestamp::from_millisecond(i64::try_from(millis).ok()?).ok()
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

struct CheckedWriter<'a, W> {
    output: &'a mut W,
    remaining: &'a mut usize,
    cancelled: &'a AtomicBool,
}

impl<'a, W> CheckedWriter<'a, W> {
    fn new(output: &'a mut W, remaining: &'a mut usize, cancelled: &'a AtomicBool) -> Self {
        Self {
            output,
            remaining,
            cancelled,
        }
    }
}

impl<W: io::Write> io::Write for CheckedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Write::write_all retries Interrupted. The publisher converts this
        // error to cancellation after the staged file has been cleaned up.
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("The report export was cancelled"));
        }
        if bytes.len() > *self.remaining {
            return Err(invalid("The snippet reports exceed the export size limit"));
        }
        let written = self.output.write(bytes)?;
        *self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
