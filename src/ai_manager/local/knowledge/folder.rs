//! Local folder review. Embedding requests only use the confirmed file manifest.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, FileStamp};

use super::source::{Filter, Ignore, Options, SyncMode};
use super::{
    ConfiguredFileTooLarge, LEGACY_MAX_SOURCES, MAX_SOURCES, pdf, saved_source_name, scan_source,
    scan_source_file, source_name,
};

mod approval;
use approval::Approval;

const LEGACY_MAX_VISITED: usize = 512;
const MAX_VISITED: usize = 2 * MAX_SOURCES;
const MAX_CORPUS_BYTES: u64 = 500 * 1024 * 1024;
const CORPUS_LIMIT: &str = "The reviewed folder exceeds 500 MiB of eligible source files. Refine the source filters and review it again.";
const MAX_DEPTH: usize = 8;
pub(in crate::ai_manager::local) const MAX_FOLDERS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ai_manager::local) struct Folder {
    pub(in crate::ai_manager::local) root: PathBuf,
    #[serde(default, skip_serializing_if = "Options::is_default")]
    pub(in crate::ai_manager::local) options: Options,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval: Option<Approval>,
    files: Vec<Approved>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Approved {
    path: PathBuf,
    digest: [u8; 32],
}

impl Folder {
    pub(super) fn prepare_backup(&mut self) {
        self.options.sync = SyncMode::Manual;
        self.options.max_file_bytes = self
            .options
            .max_file_bytes
            .filter(|limit| !limit.is_reviewed());
        self.approval = None;
        self.files.clear();
    }

    pub(super) fn index_definition(&self) -> Self {
        let mut folder = self.clone();
        folder.options.sync = SyncMode::Manual;
        folder.approval = None;
        folder
    }

    pub(in crate::ai_manager::local) fn len(&self) -> usize {
        self.files.len()
    }

    pub(super) fn uses_expanded_approval(&self) -> bool {
        self.approval.as_ref().is_some_and(Approval::large_corpus)
    }

    pub(super) fn sources(&self) -> impl Iterator<Item = (PathBuf, [u8; 32])> + '_ {
        self.files
            .iter()
            .map(|file| (self.root.join(&file.path), file.digest))
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.root.is_absolute() {
            validate_root(&self.root)?;
        } else {
            let name = saved_source_name(&self.root).map_err(|error| error.to_string())?;
            if excluded(name) || self.root.to_string_lossy().chars().any(ambiguous_character) {
                return Err("Choose a folder outside the hidden or excluded paths.".into());
            }
        }
        self.options.validate()?;
        if self.options.sync != SyncMode::Automatic && self.approval.is_some() {
            return Err("Review the folder to approve its automatic sync boundary.".into());
        }
        for file in &self.files {
            if !file
                .path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
                || !file
                    .path
                    .iter()
                    .all(|component| component.to_str().is_some_and(|name| !excluded(name)))
                || !supported(&file.path)
                || pdf::is_pdf(&file.path) && !self.options.include_pdf
            {
                return Err("Invalid reviewed folder file".into());
            }
            saved_source_name(&self.root.join(&file.path)).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

pub(crate) struct Review {
    pub(in crate::ai_manager::local) folder: Folder,
    pub(in crate::ai_manager::local) rows: Vec<Row>,
    pub(in crate::ai_manager::local) existing: bool,
    pub(in crate::ai_manager::local) missing: bool,
    boundary_changed: bool,
}

impl fmt::Debug for Review {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FolderReview")
            .field("files", &self.folder.len())
            .field("missing", &self.missing)
            .finish_non_exhaustive()
    }
}

pub(in crate::ai_manager::local) struct Row {
    pub(in crate::ai_manager::local) path: String,
    pub(in crate::ai_manager::local) status: Status,
}

#[derive(Clone, Copy)]
pub(in crate::ai_manager::local) enum Status {
    Ready { bytes: usize, change: &'static str },
    Skipped(&'static str),
    Removed,
}

pub(in crate::ai_manager::local) struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub(in crate::ai_manager::local) type ReviewReply = Receiver<Result<Arc<Review>, String>>;

pub(in crate::ai_manager::local) fn start(
    root: PathBuf,
    previous: Option<Folder>,
    available: usize,
    explicit: Vec<PathBuf>,
    options: Options,
) -> Result<(ReviewReply, Cancel), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancel);
    let (send, reply) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-knowledge-review".into())
        .spawn(move || {
            let result = scan(
                root,
                previous.as_ref(),
                available,
                &explicit,
                &stop,
                options,
                None,
            );
            let _ = send.try_send(result.map(Arc::new));
        })
        .map_err(|error| format!("Could not start the folder review ({error})."))?;
    Ok((reply, Cancel(cancel)))
}

pub(super) enum RefreshError {
    Boundary,
    Unavailable,
}

pub(super) fn refresh(
    source: &Folder,
    available: usize,
    explicit: &[PathBuf],
    cancelled: &AtomicBool,
) -> Result<Folder, RefreshError> {
    let approved = source.approval.as_ref().ok_or(RefreshError::Boundary)?;
    let review = scan(
        source.root.clone(),
        Some(source),
        available,
        explicit,
        cancelled,
        source.options.clone(),
        Some(approved),
    )
    .map_err(|_| RefreshError::Unavailable)?;
    if review.boundary_changed {
        return Err(RefreshError::Boundary);
    }
    if review.missing {
        return Err(RefreshError::Unavailable);
    }
    // A confirmed deletion or an empty file removes its old passages. An unreadable
    // file, unsafe replacement, or temporarily inaccessible subtree retains them.
    let current: HashSet<_> = review
        .folder
        .files
        .iter()
        .map(|file| path_key(&file.path))
        .collect();
    for old in &source.files {
        if cancelled.load(Ordering::Acquire) {
            return Err(RefreshError::Unavailable);
        }
        if current.contains(&path_key(&old.path)) {
            continue;
        }
        match scan_source(
            &source.root.join(&old.path),
            source.options.max_file_bytes,
            || !cancelled.load(Ordering::Acquire),
            |_, _| Ok(()),
        ) {
            Ok(document) if !document.has_passages => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(RefreshError::Unavailable),
        }
    }
    Ok(review.folder)
}

fn scan(
    root: PathBuf,
    previous: Option<&Folder>,
    available: usize,
    explicit: &[PathBuf],
    cancelled: &AtomicBool,
    options: Options,
    expected: Option<&Approval>,
) -> Result<Review, String> {
    validate_root(&root)?;
    let filter = Filter::new(&options)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("The folder review was cancelled.".into());
    }
    let mut review = Review {
        folder: Folder {
            root,
            options,
            approval: None,
            files: Vec::new(),
        },
        rows: Vec::new(),
        existing: previous.is_some(),
        missing: false,
        boundary_changed: false,
    };
    let directory = match Directory::open(&review.folder.root, false) {
        Ok(directory) => directory,
        Err(error) if previous.is_some() && error.kind() == io::ErrorKind::NotFound => {
            review.missing = true;
            review.record_removals(previous);
            return Ok(review);
        }
        Err(error) => return Err(format!("Could not open the folder ({error}).")),
    };
    #[cfg(windows)]
    if let (Some(parent), Ok(name)) = (
        review.folder.root.parent(),
        source_name(&review.folder.root),
    ) && Directory::open(parent, false)
        .and_then(|parent| parent.is_hidden_child(name))
        .map_err(|error| format!("Could not check the selected folder ({error})."))?
    {
        return Err("Choose a folder outside the hidden or excluded paths.".into());
    }
    if let Some(expected) = expected
        && !expected
            .same_directory(&directory)
            .map_err(|error| error.to_string())?
    {
        review.boundary_changed = true;
        return Ok(review);
    }
    let (ignore, ignore_digest) = if review.folder.options.respect_gitignore {
        Ignore::load(&directory)?
    } else {
        (Ignore::default(), None)
    };
    let large_corpus = expected.is_none_or(Approval::large_corpus);
    if review.folder.options.sync == SyncMode::Automatic {
        let approval = Approval::new(
            &directory,
            ignore_digest,
            review.folder.options.max_file_bytes,
            large_corpus,
        )
        .map_err(|error| error.to_string())?;
        if expected.is_some_and(|expected| expected != &approval) {
            review.boundary_changed = true;
            return Ok(review);
        }
        review.folder.approval = Some(approval);
    }
    let mut scan = Scan {
        review: &mut review,
        previous: previous
            .into_iter()
            .flat_map(|folder| &folder.files)
            .map(|file| (path_key(&file.path), file.digest))
            .collect(),
        available: if large_corpus {
            available
        } else {
            available.min(LEGACY_MAX_SOURCES)
        },
        explicit: explicit.iter().map(|path| path_key(path)).collect(),
        cancelled,
        visited: 0,
        max_visited: if large_corpus {
            MAX_VISITED
        } else {
            LEGACY_MAX_VISITED
        },
        corpus_bytes: 0,
        bounded_corpus: large_corpus,
        filter,
        ignore,
    };
    scan.visit(&directory, Path::new(""), 0)?;
    scan.active()?;
    let current = Directory::open(&review.folder.root, false)
        .and_then(|current| current.same_as(&directory))
        .map_err(|error| format!("Could not verify the reviewed folder ({error})."))?;
    if !current {
        return Err("The folder changed during review. Review it again.".into());
    }
    if review.folder.approval.is_some() && review.folder.options.respect_gitignore {
        let (_, current_digest) = Ignore::load(&directory)?;
        if current_digest != ignore_digest {
            return Err(
                "The source .gitignore changed during review. Review the folder again.".into(),
            );
        }
    }
    review.record_removals(previous);
    Ok(review)
}

impl Review {
    fn record_removals(&mut self, previous: Option<&Folder>) {
        if let Some(previous) = previous {
            let current: HashSet<_> = self
                .folder
                .files
                .iter()
                .map(|file| path_key(&file.path))
                .collect();
            for file in &previous.files {
                if !current.contains(&path_key(&file.path)) {
                    self.rows.push(Row {
                        path: display_path(&file.path),
                        status: Status::Removed,
                    });
                }
            }
        }
    }
}

struct Scan<'a> {
    review: &'a mut Review,
    previous: HashMap<PathBuf, [u8; 32]>,
    available: usize,
    explicit: HashSet<PathBuf>,
    cancelled: &'a AtomicBool,
    visited: usize,
    max_visited: usize,
    corpus_bytes: u64,
    bounded_corpus: bool,
    filter: Filter,
    ignore: Ignore,
}

impl Scan<'_> {
    fn visit(
        &mut self,
        directory: &Directory,
        relative: &Path,
        depth: usize,
    ) -> Result<(), String> {
        self.active()?;
        let remaining = self.max_visited - self.visited;
        let mut names = directory.names(remaining).map_err(|error| {
            format!(
                "Could not finish the folder review within {} entries ({error}).",
                self.max_visited
            )
        })?;
        self.visited += names.len();
        names.sort();
        for name in names {
            self.active()?;
            let path = relative.join(&name);
            let Some(name) = name.to_str() else {
                self.record(&path, Status::Skipped("File name is not UTF-8"));
                continue;
            };
            if excluded(name) || hidden_child(directory, name) {
                self.record(&path, Status::Skipped("Excluded or hidden path"));
                continue;
            }
            if let Ok(child) = directory.subdirectory(name, false) {
                if !self.filter.allows(&path, true) || !self.ignore.allows(&path, true) {
                    self.record(&path, Status::Skipped("Excluded by source rules"));
                    continue;
                }
                let same_volume = directory
                    .identity()
                    .and_then(|parent| child.identity().map(|child| child.0 == parent.0))
                    .map_err(|error| format!("Could not verify the folder boundary ({error})."))?;
                if !same_volume {
                    self.record(&path, Status::Skipped("Filesystem boundary"));
                } else if !self.review.folder.options.recursive {
                    self.record(&path, Status::Skipped("Recursive scanning is disabled"));
                } else if depth == MAX_DEPTH {
                    self.record(&path, Status::Skipped("Folder nesting limit"));
                } else {
                    self.visit(&child, &path, depth + 1)?;
                }
                continue;
            }
            self.file(directory, name, path)?;
        }
        Ok(())
    }

    fn file(&mut self, directory: &Directory, name: &str, path: PathBuf) -> Result<(), String> {
        let full = self.review.folder.root.join(&path);
        if !self.filter.allows(&path, false) || !self.ignore.allows(&path, false) {
            self.record(&path, Status::Skipped("Excluded by source rules"));
        } else if pdf::is_pdf(&path) && !self.review.folder.options.include_pdf {
            self.record(
                &path,
                Status::Skipped("PDFs are outside the approved folder formats"),
            );
        } else if !supported(&path) {
            self.record(&path, Status::Skipped("Unsupported file type"));
        } else if self.explicit.contains(&path_key(&full)) {
            self.record(
                &path,
                Status::Skipped("Already listed as an individual file"),
            );
        } else if source_name(&full).is_err() {
            self.record(&path, Status::Skipped("Invalid or oversized path"));
        } else {
            let contents = directory.open_file(name).and_then(|file| {
                let stamp = FileStamp::read(&file)?;
                let bytes = file.metadata()?.len();
                if self.bounded_corpus
                    && bytes
                        <= super::source::effective_file_size(
                            &full,
                            self.review.folder.options.max_file_bytes,
                        )
                {
                    self.corpus_bytes += bytes;
                    if self.corpus_bytes > MAX_CORPUS_BYTES {
                        return Err(io::Error::other(CORPUS_LIMIT));
                    }
                }
                let document = scan_source_file(
                    &file,
                    &full,
                    self.review.folder.options.max_file_bytes,
                    || !self.cancelled.load(Ordering::Acquire),
                    |_, _| Ok(()),
                )?;
                stamp.require_unchanged(&file)?;
                stamp.require_unchanged(&directory.open_file(name)?)?;
                Ok(document)
            });
            if self.corpus_bytes > MAX_CORPUS_BYTES {
                return Err(CORPUS_LIMIT.into());
            }
            match contents {
                Ok(document) if document.has_passages => {
                    if self.review.folder.len() == self.available {
                        return Err("The reviewed folder would exceed the store file limit. Choose a smaller folder.".into());
                    }
                    let digest = document.digest;
                    let change = match self.previous.get(&path_key(&path)) {
                        Some(previous) if *previous == digest => "Unchanged",
                        Some(_) => "Changed",
                        None => "New",
                    };
                    self.record(
                        &path,
                        Status::Ready {
                            bytes: document.bytes,
                            change,
                        },
                    );
                    self.review.folder.files.push(Approved { path, digest });
                }
                Ok(_) => self.record(&path, Status::Skipped("No indexable text")),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Err("The folder review was cancelled.".into());
                }
                Err(error)
                    if error
                        .get_ref()
                        .is_some_and(|error| error.is::<ConfiguredFileTooLarge>()) =>
                {
                    self.record(&path, Status::Skipped(ConfiguredFileTooLarge::MESSAGE));
                }
                Err(error) if let Some(reason) = pdf::failure_message(&error) => {
                    self.record(&path, Status::Skipped(reason));
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    self.record(
                        &path,
                        Status::Skipped(
                            "Invalid UTF-8, binary data, oversized file, or unsafe entry",
                        ),
                    );
                }
                Err(_) => self.record(
                    &path,
                    Status::Skipped("Unreadable, changed, or symbolic-link entry"),
                ),
            }
        }
        Ok(())
    }

    fn active(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err("The folder review was cancelled.".into())
        } else {
            Ok(())
        }
    }

    fn record(&mut self, path: &Path, status: Status) {
        self.review.rows.push(Row {
            path: display_path(path),
            status,
        });
    }
}

pub(super) fn validate_root(root: &Path) -> Result<(), String> {
    let name = source_name(root).map_err(|_| "Enter an absolute path to a folder.".to_owned())?;
    if excluded(name) || root.to_string_lossy().chars().any(ambiguous_character) {
        return Err("Choose a folder outside the hidden or excluded paths.".into());
    }
    #[cfg(windows)]
    if !matches!(root.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err("Choose a folder on a local drive.".into());
    }
    Ok(())
}

#[cfg(windows)]
fn hidden_child(directory: &Directory, name: &str) -> bool {
    directory.is_hidden_child(name).unwrap_or(false)
}

#[cfg(not(windows))]
fn hidden_child(_directory: &Directory, _name: &str) -> bool {
    false
}

fn display_path(path: &Path) -> String {
    let mut display = String::new();
    for character in path.to_string_lossy().chars() {
        if ambiguous_character(character) {
            display.extend(character.escape_unicode());
        } else {
            display.push(character);
        }
    }
    display
}

pub(super) fn same_path(left: &Path, right: &Path) -> bool {
    within(left, right) && within(right, left)
}

pub(super) fn within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    if path.to_str().is_some() && root.to_str().is_some() {
        return path_key(path).starts_with(path_key(root));
    }
    path.starts_with(root)
}

pub(super) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Some(path) = path.to_str() {
        let path = path.replace('/', "\\");
        return PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(&path).to_lowercase());
    }
    path.to_owned()
}

fn excluded(name: &str) -> bool {
    name.starts_with('.')
        || name.chars().any(ambiguous_character)
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "venv"
                | "node_modules"
                | "vendor"
                | "build"
                | "target"
                | "dist"
                | "out"
                | "coverage"
                | "__pycache__"
        )
}

fn ambiguous_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}' | '\u{feff}'
        )
}

fn supported(path: &Path) -> bool {
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "txt"
                | "pdf"
                | "md"
                | "markdown"
                | "adoc"
                | "asciidoc"
                | "rst"
                | "json"
                | "jsonl"
                | "xml"
                | "yaml"
                | "yml"
                | "toml"
                | "ini"
                | "cfg"
                | "conf"
                | "properties"
                | "csv"
                | "tsv"
                | "sh"
                | "bash"
                | "zsh"
                | "fish"
                | "bat"
                | "cmd"
                | "awk"
                | "ps1"
                | "psm1"
                | "psd1"
                | "py"
                | "pyw"
                | "pyi"
                | "rb"
                | "pl"
                | "php"
                | "lua"
                | "java"
                | "kt"
                | "kts"
                | "groovy"
                | "gradle"
                | "gvy"
                | "gy"
                | "gsh"
                | "scala"
                | "sc"
                | "cs"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "html"
                | "htm"
                | "css"
                | "scss"
                | "sass"
                | "less"
                | "vue"
                | "svelte"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "cxx"
                | "hpp"
                | "hxx"
                | "hh"
                | "inl"
                | "go"
                | "rs"
                | "swift"
                | "sql"
        )
    } else {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "readme" | "license" | "dockerfile" | "makefile"
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::source::{DEFAULT_TEXT_BYTES, FileSizeLimit};
    use super::*;

    fn review(root: &Path, options: Options) -> Review {
        scan(
            root.to_owned(),
            None,
            8,
            &[],
            &AtomicBool::new(false),
            options,
            None,
        )
        .unwrap()
    }

    fn status(review: &Review, name: &str) -> Status {
        review
            .rows
            .iter()
            .find(|row| row.path == name)
            .unwrap()
            .status
    }

    #[test]
    fn new_folder_review_expands_corpus_without_expanding_old_background_approval() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let options = Options {
            sync: SyncMode::Automatic,
            ..Options::default()
        };
        fs::write(root.join("first.txt"), "Previously approved").unwrap();
        let mut old = serde_json::to_value(review(&root, options.clone()).folder).unwrap();
        old["approval"]
            .as_object_mut()
            .unwrap()
            .remove("large_corpus");
        let old: Folder = serde_json::from_value(old).unwrap();
        for index in 1..MAX_SOURCES {
            fs::write(root.join(format!("source-{index}.txt")), "Indexable text").unwrap();
        }
        let stopped = AtomicBool::new(false);
        assert!(matches!(
            refresh(&old, MAX_SOURCES, &[], &stopped),
            Err(RefreshError::Unavailable)
        ));
        let reviewed = scan(root, Some(&old), MAX_SOURCES, &[], &stopped, options, None).unwrap();
        assert_eq!(reviewed.folder.len(), MAX_SOURCES);
        let refresh = refresh(&reviewed.folder, MAX_SOURCES, &[], &stopped)
            .ok()
            .unwrap();
        assert_eq!(refresh.files, reviewed.folder.files);
    }

    #[test]
    fn folder_review_applies_the_inclusive_byte_limit_before_decoding_or_extracting() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let pdf = pdf::fixture(&["A reviewed PDF page"]);
        fs::write(root.join("exact.txt"), "éééé").unwrap();
        fs::write(root.join("over.txt"), "ééééa").unwrap();
        fs::write(root.join("page.pdf"), &pdf).unwrap();
        fs::write(
            root.join("format-cap.txt"),
            "x".repeat(DEFAULT_TEXT_BYTES + 1),
        )
        .unwrap();
        let restricted = review(
            &root,
            Options {
                include_pdf: true,
                max_file_bytes: Some(FileSizeLimit::Legacy(8)),
                ..Options::default()
            },
        );
        assert_eq!(restricted.folder.len(), 1);
        assert!(matches!(
            status(&restricted, "exact.txt"),
            Status::Ready { bytes: 8, .. }
        ));
        for name in ["over.txt", "page.pdf", "format-cap.txt"] {
            assert!(matches!(
                status(&restricted, name),
                Status::Skipped(ConfiguredFileTooLarge::MESSAGE)
            ));
        }

        let pdf_boundary = review(
            &root,
            Options {
                include_pdf: true,
                max_file_bytes: Some(FileSizeLimit::reviewed(pdf.len() as u64)),
                ..Options::default()
            },
        );
        assert!(
            matches!(status(&pdf_boundary, "page.pdf"), Status::Ready { bytes, .. } if bytes == pdf.len())
        );
        let format_boundary = review(
            &root,
            Options {
                max_file_bytes: Some(FileSizeLimit::Legacy(pdf::DEFAULT_FILE_BYTES as u64)),
                ..Options::default()
            },
        );
        assert!(matches!(
            status(&format_boundary, "over.txt"),
            Status::Ready { bytes: 9, .. }
        ));
        assert!(matches!(
            status(&format_boundary, "format-cap.txt"),
            Status::Skipped("Invalid UTF-8, binary data, oversized file, or unsafe entry")
        ));
    }

    #[test]
    fn automatic_folder_uses_the_approved_cap_and_retains_passages_from_oversized_replacements() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let old = root.join("old.txt");
        let new = root.join("new.txt");
        fs::write(&old, "Previously valid").unwrap();
        let approved = review(
            &root,
            Options {
                sync: SyncMode::Automatic,
                max_file_bytes: Some(FileSizeLimit::Legacy(16)),
                ..Options::default()
            },
        )
        .folder;
        assert_eq!(approved.len(), 1);
        let receipt = serde_json::to_value(&approved).unwrap();
        assert_eq!(receipt["approval"]["max_file_bytes"], 16);
        let approved: Folder = serde_json::from_value(receipt).unwrap();
        let stopped = AtomicBool::new(false);
        fs::write(&new, "The new file is too large").unwrap();
        let current = refresh(&approved, 8, &[], &stopped).ok().unwrap();
        assert!(current.sources().map(|(path, _)| path).eq([old.clone()]));

        // An oversized replacement made entirely of whitespace must retain its old passages.
        fs::write(&old, " ".repeat(17)).unwrap();
        assert!(matches!(
            refresh(&approved, 8, &[], &stopped),
            Err(RefreshError::Unavailable)
        ));
        let mut unreviewed = approved.clone();
        unreviewed.options.max_file_bytes = Some(FileSizeLimit::Legacy(32));
        assert!(matches!(
            refresh(&unreviewed, 8, &[], &stopped),
            Err(RefreshError::Boundary)
        ));

        fs::remove_file(old).unwrap();
        fs::write(&new, "Now small enough").unwrap();
        let current = refresh(&approved, 8, &[], &stopped).ok().unwrap();
        assert!(current.sources().map(|(path, _)| path).eq([new]));
        let legacy = review(
            &root,
            Options {
                sync: SyncMode::Automatic,
                ..Options::default()
            },
        )
        .folder;
        let receipt = serde_json::to_value(&legacy).unwrap();
        assert!(receipt["approval"].get("max_file_bytes").is_none());
        assert!(refresh(&serde_json::from_value(receipt).unwrap(), 8, &[], &stopped).is_ok());
    }

    #[test]
    fn larger_folder_sources_require_a_new_typed_automatic_approval() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let text = root.join("notes.txt");
        let document = root.join("manual.pdf");
        let text_content = format!("{}Ready to index", "\n".repeat(DEFAULT_TEXT_BYTES));
        fs::write(&text, &text_content).unwrap();
        let pdf_content = pdf::large_fixture(pdf::DEFAULT_FILE_BYTES + 1);
        fs::write(&document, &pdf_content).unwrap();
        let legacy_limit = FileSizeLimit::Legacy(pdf::DEFAULT_FILE_BYTES as u64);
        let legacy = review(
            &root,
            Options {
                sync: SyncMode::Automatic,
                include_pdf: true,
                max_file_bytes: Some(legacy_limit),
                ..Options::default()
            },
        )
        .folder;
        assert_eq!(legacy.len(), 0);
        let mut legacy: Folder =
            serde_json::from_value(serde_json::to_value(legacy).unwrap()).unwrap();
        let stopped = AtomicBool::new(false);
        assert_eq!(refresh(&legacy, 8, &[], &stopped).ok().unwrap().len(), 0);
        legacy.options.max_file_bytes = Some(FileSizeLimit::reviewed(legacy_limit.bytes()));
        assert!(matches!(
            refresh(&legacy, 8, &[], &stopped),
            Err(RefreshError::Boundary)
        ));

        let limit = pdf_content.len() as u64;
        let current = review(
            &root,
            Options {
                max_file_bytes: Some(FileSizeLimit::reviewed(limit)),
                ..legacy.options
            },
        )
        .folder;
        assert_eq!(current.len(), 2);
        let wire = serde_json::to_value(&current).unwrap();
        assert_eq!(
            wire["approval"]["max_file_bytes"],
            serde_json::json!({"extended_bytes": limit})
        );
        let restored: Folder = serde_json::from_value(wire).unwrap();
        assert_eq!(
            refresh(&restored, 8, &[], &stopped).ok().unwrap().files,
            current.files
        );

        let mut oversized = pdf_content;
        oversized.push(b' ');
        fs::write(document, oversized).unwrap();
        assert!(matches!(
            refresh(&restored, 8, &[], &stopped),
            Err(RefreshError::Unavailable)
        ));
        assert_eq!(restored.files, current.files);
    }
}
