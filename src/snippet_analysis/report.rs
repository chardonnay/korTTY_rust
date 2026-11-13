//! Bounded before- and after-apply reports from immutable history snapshots.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use zip::write::SimpleFileOptions;

use crate::private_files::{Directory, EntryIdentity};
use crate::session_journal::export_branding::PdfBranding;
use crate::session_journal::export_output::valid_component;
use crate::snippets::SnippetId;

use super::model::{ApplyRun, ApplyStatus, History, Record, Selection, Subject};

mod markdown;
mod post;
mod render;
#[cfg(test)]
mod tests;

pub(crate) use super::model::ReportFormat as Format;
pub(crate) use post::Post;

pub(crate) const MAX_REPORTS: usize = 256;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FOLDER_BYTES: usize = 1024;
const MAX_FOLDER_DEPTH: usize = 32;

impl Format {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Pdf => "pdf",
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Html => "HTML",
            Self::Pdf => "PDF",
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

impl Packing {
    pub(crate) fn extension(self, format: Format) -> &'static str {
        if self == Self::Zip {
            "zip"
        } else {
            format.extension()
        }
    }
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
    pub(crate) branding: PdfBranding,
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
    pub(crate) selection: Selection,
    pub(crate) post: Option<Post>,
}

impl Item {
    pub(crate) fn batch(
        history: &History,
        folder_path: String,
        current_hash: String,
    ) -> Option<Self> {
        let current = history.current()?;
        let direct = current
            .apply_runs
            .iter()
            .find(|run| run.status == ApplyStatus::Accepted)
            .map(|run| (current, run.id));
        let verified = || {
            let verification = current.verification.as_ref()?;
            let pre = history
                .records
                .iter()
                .find(|record| record.id == verification.previous_record_id)?;
            pre.apply_runs
                .iter()
                .find(|run| run.id == verification.run_id && run.status == ApplyStatus::Accepted)
                .map(|run| (pre, run.id))
        };
        let (record, post) = match direct.or_else(verified) {
            Some((record, run_id)) => (record, Post::capture(record, run_id, &history.records)),
            None => (current, None),
        };
        Some(Self {
            subject: history.subject,
            record: record.clone(),
            folder_path,
            current_hash: Some(current_hash),
            selection: record
                .apply_runs
                .first()
                .map_or_else(Selection::default, |run| run.selected.clone()),
            post,
        })
    }

    pub(crate) fn retained_source_bytes(&self) -> usize {
        self.record
            .source
            .content
            .as_ref()
            .map_or(0, |source| source.len())
            + self
                .record
                .apply_runs
                .iter()
                .map(|run| run.before.len() + run.after.len())
                .sum::<usize>()
    }

    fn post_run(&self) -> Option<&ApplyRun> {
        let run_id = self.post.as_ref()?.run_id;
        self.record.apply_runs.iter().find(|run| run.id == run_id)
    }

    fn selected(&self, id: &str) -> bool {
        self.post_run()
            .map_or(&self.selection, |run| &run.selected)
            .contains(id)
    }

    fn run_number(&self, id: SnippetId) -> usize {
        self.record
            .apply_runs
            .iter()
            .position(|run| run.id == id)
            .map_or(0, |index| self.record.apply_runs.len() - index)
    }

    fn stale(&self) -> bool {
        !self.record.source.sha256.is_empty()
            && self.current_hash.as_ref().is_some_and(|hash| {
                hash != &self.record.source.sha256
                    && self
                        .post_run()
                        .is_none_or(|run| hash != &run.before_sha256 && hash != &run.after_sha256)
            })
    }
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) exported: usize,
    pub(crate) skipped: usize,
    pub(crate) failed_diagrams: usize,
    pub(crate) written_files: Vec<PathBuf>,
    pub(crate) warning: Option<String>,
    pub(crate) folder: ExportFolder,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Progress {
    Rendering {
        completed: usize,
        total: usize,
        last: Option<String>,
    },
    Publishing {
        total: usize,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ExportFolder {
    path: PathBuf,
    identity: EntryIdentity,
}

impl ExportFolder {
    fn capture(path: &Path, directory: &Directory) -> io::Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            identity: directory.identity()?,
        })
    }

    pub(crate) fn current_path(&self) -> io::Result<&Path> {
        if Directory::open(&self.path, false)?.identity()? != self.identity {
            return Err(invalid("The export folder changed after publication"));
        }
        Ok(&self.path)
    }
}

pub(crate) fn formatted_destination(
    path: PathBuf,
    format: Format,
    packing: Packing,
) -> io::Result<PathBuf> {
    let (_, name) = destination(&path)?;
    let raw_name = path
        .as_os_str()
        .to_str()
        .and_then(|text| text.rsplit(std::path::is_separator).next());
    // Do not treat an explicit trailing separator or "." as a file name.
    if raw_name != Some(name) {
        return Err(invalid("Enter a safe report export filename"));
    }
    let extension = packing.extension(format);
    if name
        .rsplit_once('.')
        .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case(extension))
    {
        return Ok(path);
    }
    let target = path.with_file_name(format!("{name}.{extension}"));
    destination(&target)?;
    Ok(target)
}

#[cfg(test)]
pub(crate) fn export(
    target: PathBuf,
    reports: Vec<Item>,
    options: Options,
    cancelled: &AtomicBool,
) -> io::Result<Outcome> {
    export_with_progress(target, reports, options, cancelled, |_| {})
}

pub(crate) fn export_with_progress(
    target: PathBuf,
    reports: Vec<Item>,
    options: Options,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> io::Result<Outcome> {
    check_cancelled(cancelled)?;
    validate_selection(&reports, &options)?;
    let (folder, name) = destination(&target)?;
    let directory = Directory::open(folder, false)?;
    let export_folder = ExportFolder::capture(folder, &directory)?;
    if directory.has_entry(name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The report export destination already exists",
        ));
    }
    let total = reports.len();
    let batch = options.packing != Packing::Single;
    if batch {
        progress(Progress::Rendering {
            completed: 0,
            total,
            last: None,
        });
    }
    let mut remaining = MAX_OUTPUT_BYTES;
    let (mut assets, markdown) =
        if options.format == Format::Markdown && options.packing != Packing::Zip {
            match markdown::prepare(
                &directory,
                name,
                &reports,
                options.packing,
                &mut remaining,
                cancelled,
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    check_cancelled(cancelled)?;
                    return Err(error);
                }
            }
        } else {
            (None, Vec::new())
        };
    let check = || check_export_directory(&directory, folder, cancelled);
    let mut failed_diagrams = 0;
    let write = |output: &mut File| {
        let mut completed = 0;
        failed_diagrams = write_output(
            output,
            &reports,
            &options,
            &markdown,
            &mut remaining,
            cancelled,
            &mut |item| {
                if batch {
                    completed += 1;
                    let mut name = render::single_line(&item.record.source.snippet_name);
                    name.truncate(name.floor_char_boundary(200));
                    progress(Progress::Rendering {
                        completed,
                        total,
                        last: (!name.is_empty()).then_some(name),
                    });
                }
            },
        )?;
        if batch {
            progress(Progress::Publishing { total });
        }
        Ok(())
    };
    let publication = match &mut assets {
        Some(stage) => stage
            .write_file(name, write)
            .and_then(|()| stage.publish(check)),
        None => directory.publish_checked(name, write, check),
    };
    if let Err(error) = publication {
        if let Some(stage) = &mut assets
            && let Err(rollback) = stage.cleanup(false)
        {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}. Markdown export cleanup could not finish ({rollback}). Recovery folder: {}",
                    stage.recovery_path(folder).display()
                ),
            ));
        }
        check_cancelled(cancelled)?;
        return Err(error);
    }
    let cleanup = assets.as_mut().map_or(Ok(()), |stage| stage.cleanup(true));
    let sync = directory.sync();
    let mut written_files = vec![target.clone()];
    written_files.extend(markdown.iter().filter_map(|link| match link {
        Some(markdown::Link::Image(name)) => Some(target.with_file_name(name)),
        _ => None,
    }));
    Ok(Outcome {
        exported: reports.len(),
        skipped: options.skipped.len(),
        failed_diagrams,
        written_files,
        warning: cleanup.and(sync).err().map(|error| {
            format!("The report was saved. Final export directory cleanup or synchronization failed ({error}).")
        }),
        folder: export_folder,
    })
}

/// Writes a retained script verbatim to a new file, without a report wrapper.
pub(crate) fn export_script(
    target: PathBuf,
    script: &str,
    cancelled: &AtomicBool,
) -> io::Result<Outcome> {
    check_cancelled(cancelled)?;
    if script.is_empty() || script.len() > super::model::MAX_CONTENT_BYTES {
        return Err(invalid(
            "The saved script is unavailable or exceeds the export limit",
        ));
    }
    let (folder, name) = destination(&target)?;
    let directory = Directory::open(folder, false)?;
    let export_folder = ExportFolder::capture(folder, &directory)?;
    let published = directory.publish_checked(
        name,
        |file| file.write_all(script.as_bytes()),
        || check_export_directory(&directory, folder, cancelled),
    );
    if let Err(error) = published {
        check_cancelled(cancelled)?;
        return Err(error);
    }
    let warning = directory.sync().err().map(|error| {
        format!("The script was saved. Final export directory synchronization failed ({error}).")
    });
    Ok(Outcome {
        exported: 1,
        skipped: 0,
        failed_diagrams: 0,
        written_files: vec![target],
        warning,
        folder: export_folder,
    })
}

fn check_export_directory(
    directory: &Directory,
    folder: &Path,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    if !directory.same_as(&Directory::open(folder, false)?)? {
        return Err(invalid("The selected export folder changed"));
    }
    check_cancelled(cancelled)
}

fn validate_selection(reports: &[Item], options: &Options) -> io::Result<()> {
    if reports
        .iter()
        .any(|item| item.post.is_some() && item.post_run().is_none())
    {
        return Err(invalid(
            "The selected apply run does not exist in the report snapshot",
        ));
    }
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
    markdown: &[Option<markdown::Link>],
    remaining: &mut usize,
    cancelled: &AtomicBool,
    rendered: &mut impl FnMut(&Item),
) -> io::Result<usize> {
    if options.packing != Packing::Zip {
        return render::write(
            &mut CheckedWriter::new(output, remaining, cancelled),
            reports,
            options,
            options.packing,
            markdown,
            cancelled,
            rendered,
        );
    }

    let mut archive = zip::ZipWriter::new(output);
    let entry_options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    let mut failed_diagrams = 0;
    for (index, item) in reports.iter().enumerate() {
        check_cancelled(cancelled)?;
        let name = archive_name(index, item, options.format)?;
        let mut image = None;
        let mut link = None;
        if options.format == Format::Markdown
            && let Some(diagram) = &item.record.diagram
        {
            match diagram.report_png() {
                Ok(png) => {
                    let (prefix, file) = name
                        .rsplit_once('/')
                        .map_or(("", name.as_str()), |(folder, file)| {
                            (&name[..folder.len() + 1], file)
                        });
                    let asset = markdown::image_name(file, None)?;
                    image = Some((format!("{prefix}{asset}"), png));
                    link = Some(markdown::Link::Image(asset));
                }
                Err(error) => link = Some(markdown::Link::Failed(error)),
            }
        }
        archive.start_file(name, entry_options)?;
        failed_diagrams += render::write(
            &mut CheckedWriter::new(&mut archive, remaining, cancelled),
            std::slice::from_ref(item),
            options,
            Packing::Zip,
            std::slice::from_ref(&link),
            cancelled,
            &mut |_| {},
        )?;
        if let Some((name, png)) = image {
            archive.start_file(name, entry_options)?;
            CheckedWriter::new(&mut archive, remaining, cancelled).write_all(&png)?;
        }
        rendered(item);
    }
    if !options.skipped.is_empty() {
        check_cancelled(cancelled)?;
        archive.start_file("skipped.txt", entry_options)?;
        let mut writer = CheckedWriter::new(&mut archive, remaining, cancelled);
        writeln!(writer, "Snippets without a stored analysis")?;
        for name in &options.skipped {
            writeln!(writer, "- {}", render::single_line(name))?;
        }
    }
    check_cancelled(cancelled)?;
    archive.finish()?;
    Ok(failed_diagrams)
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
