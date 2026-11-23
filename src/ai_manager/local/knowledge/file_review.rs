//! Cancellable preflight for individually selected knowledge sources.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::Receiver;

use super::{
    ConfiguredFileTooLarge, MAX_SOURCES, folder, new_source_file_limit, pdf, read_source_digest,
    scan_source_file, source, text_stream, with_source_file,
};

#[derive(Debug)]
pub(crate) struct Review {
    pub(in crate::ai_manager::local) rows: Vec<Row>,
    pub(in crate::ai_manager::local) file_limit: Option<u64>,
}

#[derive(Debug)]
pub(in crate::ai_manager::local) struct Row {
    pub(in crate::ai_manager::local) path: PathBuf,
    pub(in crate::ai_manager::local) status: Status,
}

#[derive(Debug)]
pub(in crate::ai_manager::local) enum Status {
    Ready { bytes: usize, digest: [u8; 32] },
    Skipped(&'static str),
}

impl Review {
    pub(in crate::ai_manager::local) fn has_ready_files(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.status, Status::Ready { .. }))
    }

    pub(in crate::ai_manager::local) fn has_oversized_files(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.status, Status::Skipped(ConfiguredFileTooLarge::MESSAGE)))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Confirmed {
    pub(in crate::ai_manager::local) paths: Vec<PathBuf>,
    pub(in crate::ai_manager::local) file_limit: Option<u64>,
}

pub(in crate::ai_manager::local) fn start(
    paths: Vec<PathBuf>,
    file_limit: Option<u64>,
) -> Result<(Receiver<Result<Arc<Review>, String>>, folder::Cancel), String> {
    if paths.len() > MAX_SOURCES {
        return Err(format!("Select up to {MAX_SOURCES} files per review."));
    }
    source::validate_file_size(file_limit)?;
    folder::start_review(move |cancel| {
        let paths = paths.into_iter().map(|path| {
            let limit = limit(&path, file_limit);
            (path, Some(limit))
        });
        let rows = scan(paths, cancel)?;
        Ok(Arc::new(Review { rows, file_limit }))
    })
}

pub(in crate::ai_manager::local) fn confirm(
    review: Arc<Review>,
) -> Result<(Receiver<Result<Confirmed, String>>, folder::Cancel), String> {
    folder::start_review(move |cancel| {
        let mut files = Vec::new();
        for row in &review.rows {
            let Status::Ready { digest, .. } = row.status else {
                continue;
            };
            let actual =
                read_source_digest(&row.path, Some(limit(&row.path, review.file_limit)), || {
                    active(cancel)
                })?;
            if actual != digest {
                return Err(format!(
                    "The source changed after the scan: {}. Review it again.",
                    row.path.display()
                ));
            }
            files.push(row.path.clone());
        }
        Ok(Confirmed {
            paths: files,
            file_limit: review.file_limit,
        })
    })
}

fn limit(path: &std::path::Path, bytes: Option<u64>) -> source::FileSizeLimit {
    bytes.map_or_else(
        || new_source_file_limit(path),
        |bytes| source::FileSizeLimit::reviewed_for_text(bytes, !pdf::is_pdf(path)),
    )
}

pub(super) fn scan(
    paths: impl IntoIterator<Item = (PathBuf, Option<source::FileSizeLimit>)>,
    cancel: &AtomicBool,
) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut scanned_bytes = 0;
    for (path, limit) in paths {
        active(cancel)?;
        let status = if !folder::supported(&path) {
            Status::Skipped("Unsupported file type")
        } else {
            let result = with_source_file(
                &path,
                |error| error,
                |file| {
                    let bytes = file.metadata()?.len();
                    if bytes > source::effective_file_size(&path, limit) {
                        return Err(io::Error::other(ConfiguredFileTooLarge));
                    }
                    scanned_bytes += bytes;
                    if scanned_bytes > source::MAX_CORPUS_BYTES as u64 {
                        return Err(io::Error::other("File review scan limit exceeded"));
                    }
                    scan_source_file(
                        file,
                        &path,
                        limit,
                        || !cancel.load(Ordering::Relaxed),
                        |_, _| Ok(()),
                    )
                },
            );
            if scanned_bytes > source::MAX_CORPUS_BYTES as u64 {
                return Err("The selected files exceed 500 MiB of eligible source files. Select a smaller batch.".into());
            }
            match result {
                Ok(document) if document.has_passages => Status::Ready {
                    bytes: document.bytes,
                    digest: document.digest,
                },
                Ok(_) => Status::Skipped("No indexable text"),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Err("The file review was cancelled.".into());
                }
                Err(error) => Status::Skipped(problem(&error)),
            }
        };
        rows.push(Row { path, status });
    }
    Ok(rows)
}

fn active(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("The file review was cancelled.".into())
    } else {
        Ok(())
    }
}

fn problem(error: &io::Error) -> &'static str {
    if error
        .get_ref()
        .is_some_and(|cause| cause.is::<ConfiguredFileTooLarge>())
    {
        return ConfiguredFileTooLarge::MESSAGE;
    }
    if let Some(reason) = pdf::failure_message(error) {
        return reason;
    }
    if let Some(cause) = error.get_ref() {
        if cause.is::<std::str::Utf8Error>() {
            return "File is not valid UTF-8";
        }
        if cause.is::<text_stream::BinaryText>() {
            return "File contains binary control characters";
        }
    }
    match error.kind() {
        io::ErrorKind::NotFound => "File not found",
        io::ErrorKind::InvalidData => "Invalid UTF-8, binary data, oversized file, or unsafe entry",
        _ => "Unreadable, changed, or symbolic-link entry",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::local::knowledge::Store;
    use iced::futures::executor::block_on;

    #[test]
    fn enlarged_file_limit_is_reviewed_and_saved_with_the_source() {
        let fixture = crate::snippets::tests::Profile::new();
        let path = fixture.0.canonicalize().unwrap().join("notes.txt");
        std::fs::write(&path, "Knowledge fixture text").unwrap();

        let (reply, _scan) = start(vec![path.clone()], Some(4)).unwrap();
        let review = block_on(reply.recv()).unwrap().unwrap();
        assert!(review.has_oversized_files());
        assert!(!review.has_ready_files());

        let (reply, _scan) = start(vec![path.clone()], Some(64)).unwrap();
        let review = block_on(reply.recv()).unwrap().unwrap();
        let (reply, _confirmation) = confirm(review).unwrap();
        let confirmed = block_on(reply.recv()).unwrap().unwrap();
        let store = Store::new("Fixture store")
            .unwrap()
            .review_source_files(&confirmed.paths, confirmed.file_limit)
            .unwrap();
        assert_eq!(store.sources, [path.clone()]);
        assert_eq!(store.file_size_limit(&path), Some(64));
    }
}
