//! Bounded local content search over a journal catalog snapshot.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use roxmltree::Node;

use crate::private_files::Directory;
use crate::sftp_window::LocalSource;

use super::capture::{ContentMatch, content_match};
use super::deletion::DeletionIdentity;
use super::tree::{Filter, matches_metadata};
use super::{
    Catalog, MAX_CATALOG_BYTES, MAX_CATALOG_ROWS, MAX_DOCUMENT_BYTES, Summary, bounded_field,
    child, document_id, entries, invalid, number, open_document, parse_document, scalar_text,
    source_text,
};

const MAX_SEARCH_LOG_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct SearchResult {
    /// Indices into the catalog used to start this search. Metadata matches are independent.
    pub matching_indices: HashSet<usize>,
    pub skipped: usize,
    pub partial: bool,
}

struct Budget {
    xml: u64,
    log: u64,
}

/// Scans entries and capture logs for a literal query already normalized by `tree::prepare_query`.
pub(crate) fn scan(
    root: &Path,
    catalog: &Catalog,
    filter: Filter,
    normalized_query: &str,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<SearchResult> {
    scan_bounded(
        root,
        catalog,
        filter,
        normalized_query,
        Budget {
            xml: MAX_CATALOG_BYTES,
            log: MAX_SEARCH_LOG_BYTES,
        },
        cancelled,
    )
}

fn scan_bounded(
    root: &Path,
    catalog: &Catalog,
    filter: Filter,
    query: &str,
    mut budget: Budget,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<SearchResult> {
    check_cancelled(cancelled)?;
    let mut result = SearchResult {
        partial: catalog.limited
            || catalog.skipped > 0
            || catalog.journals.len() > MAX_CATALOG_ROWS,
        ..SearchResult::default()
    };
    if query.is_empty() || catalog.journals.is_empty() {
        return Ok(result);
    }
    let directory = Directory::open(root, false)?;
    for (index, row) in catalog.journals.iter().take(MAX_CATALOG_ROWS).enumerate() {
        check_cancelled(cancelled)?;
        if !filter.matches(row) || matches_metadata(row, query) {
            continue;
        }
        match journal_match(root, &directory, row, query, &mut budget, cancelled) {
            Ok(matched) => {
                result.partial |= matched.partial;
                if matched.found {
                    result.matching_indices.insert(index);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(_) => {
                result.partial = true;
                result.skipped += 1;
            }
        }
    }
    check_cancelled(cancelled)?;
    Ok(result)
}

fn journal_match(
    root: &Path,
    directory: &Directory,
    row: &Summary,
    query: &str,
    budget: &mut Budget,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<ContentMatch> {
    let expected = row
        .deletion_identity
        .as_ref()
        .filter(|_| row.directory.parent() == Some(root))
        .ok_or_else(changed)?;
    let source = open_document(&row.directory)?;
    let observed = DeletionIdentity::capture(directory, &source)?;
    if !expected.same_location(&observed) {
        return Err(changed());
    }
    let xml = read_search_xml(&source, &mut budget.xml)?;
    check_cancelled(cancelled)?;
    let document = parse_document(&xml)?;
    let id = document_id(&document)?;
    if id != row.id {
        return Err(changed());
    }
    let meta = child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))?;
    let mut result = ContentMatch {
        found: false,
        partial: false,
    };
    for entry in entries(&document) {
        check_cancelled(cancelled)?;
        match entry_matches(entry, query, cancelled) {
            Ok(true) => {
                result.found = true;
                break;
            }
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(_) => result.partial = true,
        }
    }
    if !result.found {
        let declared_parts = number::<u32>(meta, "logParts")?.unwrap_or(1);
        let log = content_match(
            &source,
            id,
            declared_parts,
            query,
            &mut budget.log,
            cancelled,
        )?;
        result.found = log.found;
        result.partial |= log.partial;
    }
    check_cancelled(cancelled)?;
    // Live journals may publish a new XML document between the catalog and the
    // search. Keep the read tied to one observed version at the same location.
    observed.require_document(source.file())?;
    let current = open_document(&row.directory)?;
    let current_root = Directory::open(root, false)?;
    if DeletionIdentity::capture(&current_root, &current)? != observed {
        return Err(changed());
    }
    Ok(result)
}

fn read_search_xml(source: &LocalSource, remaining: &mut u64) -> io::Result<String> {
    let size = source.size();
    let maximum_read = size.saturating_add(1);
    if size > MAX_DOCUMENT_BYTES || maximum_read > *remaining {
        return Err(invalid("Journal content search reached its document limit"));
    }
    // Charge the bounded read even if the file becomes unreadable partway through.
    *remaining -= maximum_read;
    let mut file_budget = size;
    source_text(source, size, &mut file_budget)
}

fn entry_matches(
    entry: Node<'_, '_>,
    query: &str,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<bool> {
    for name in ["title", "text", "userNote", "aiDescription"] {
        if bounded_field(entry, name)?.to_lowercase().contains(query) {
            return Ok(true);
        }
    }
    for tag in child(entry, "aiTags")
        .into_iter()
        .flat_map(|tags| tags.children())
        .filter(|tag| tag.has_tag_name("tag"))
    {
        check_cancelled(cancelled)?;
        if scalar_text(tag)?.to_lowercase().contains(query) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn changed() -> io::Error {
    invalid("The journal changed. Refresh the journal list.")
}

fn check_cancelled(cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Journal content search was cancelled",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
