//! Assembles an HTML bundle from one validated journal snapshot.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::io;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use roxmltree::Node;

use crate::private_files::Directory;

use super::super::capture::bundle as capture;
use super::super::viewer::screenshot::open_published_image;
use super::super::{DOCUMENT, MAX_PNG_BYTES};
use super::{
    Entry, Options, Selection, bounded_field, child, document_id, entries, invalid, number,
    parse_document,
};

mod html;
mod xml;

const MAX_CAPTURE_INPUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SCREENSHOT_REFERENCES: usize = 4096;
const MAX_INLINE_PNG_BYTES: usize = 24 * 1024 * 1024;

type WriteMember<'a> = dyn FnMut(&str, &[u8]) -> io::Result<()> + 'a;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BundleStats {
    pub command_count: u64,
    pub error_count: u64,
    pub screenshot_count: u64,
    pub skipped_assets: usize,
    pub capture_incomplete: bool,
}

#[cfg(test)]
impl super::Snapshot {
    pub(crate) fn write_html_bundle(
        &self,
        options: &Options,
        links_allowed: bool,
        emit: impl FnMut(&str, &[u8]) -> io::Result<()>,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<BundleStats> {
        check_cancelled(cancelled)?;
        self.select_for_export(&options.filter, None, cancelled)?
            .write_html_bundle(options, links_allowed, emit, cancelled)
    }
}

impl Selection<'_> {
    /// Emits relative members into the publisher's private archive stage.
    /// A caller must discard its stage if any read, cancellation or identity check fails.
    pub(crate) fn write_html_bundle(
        &self,
        options: &Options,
        links_allowed: bool,
        mut emit: impl FnMut(&str, &[u8]) -> io::Result<()>,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<BundleStats> {
        let (_, stats) = self.render_html(options, links_allowed, Some(&mut emit), cancelled)?;
        Ok(stats)
    }

    /// Renders a self-contained page for a reviewed local browser handoff.
    pub(crate) fn render_browser_html(
        &self,
        options: &Options,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<(String, BundleStats)> {
        self.render_html(options, false, None, cancelled)
    }

    fn render_html(
        &self,
        options: &Options,
        links_allowed: bool,
        mut archive: Option<&mut WriteMember<'_>>,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<(String, BundleStats)> {
        check_cancelled(cancelled)?;
        self.require_matches(&options.filter)?;
        let snapshot = self.snapshot;
        let selected = &self.entries;
        let filtered = options.filter.is_active();
        let source = snapshot.open_current_source()?;
        let original = snapshot
            .xml
            .as_deref()
            .ok_or_else(|| invalid("Missing verified journal bundle XML"))?;
        let document = parse_document(original)?;
        let id = document_id(&document)?;
        let meta = child(document.root_element(), "meta")
            .ok_or_else(|| invalid("Missing session journal metadata"))?;
        let nodes: Vec<_> = entries(&document).collect();
        let ranges = filtered.then(|| {
            capture::Ranges::new(
                selected
                    .iter()
                    .map(|entry| (entry.log_start, entry.log_end)),
            )
        });
        let matches_time = |moment| options.filter.matches_capture_time(moment);
        let selection = ranges.as_ref().map(|ranges| capture::Selection {
            ranges,
            matches_time: &matches_time,
        });
        let mut log = html::LogPreview::new(selected);
        let mut capture_budget = MAX_CAPTURE_INPUT_BYTES;
        let captured = capture::export(
            &source,
            id,
            number(meta, "logParts")?.unwrap_or(1),
            selection,
            &mut capture_budget,
            cancelled,
            |output| match output {
                capture::Output::Part(part) => match &mut archive {
                    Some(emit) => emit(part.name, part.bytes),
                    None => Ok(()),
                },
                capture::Output::Record(row) => {
                    log.push(
                        &row.record,
                        row.screenshot.as_ref().map(capture::ScreenshotRef::as_str),
                    );
                    Ok(())
                }
            },
        )?;
        if captured.partial {
            log.mark_incomplete();
        }
        let mut stats = if filtered {
            BundleStats {
                command_count: captured.command_count,
                error_count: count_errors(&nodes, selected)?,
                screenshot_count: selected
                    .iter()
                    .filter(|entry| entry.kind == "SCREENSHOT")
                    .count() as u64,
                skipped_assets: 0,
                capture_incomplete: captured.partial,
            }
        } else {
            BundleStats {
                command_count: snapshot.header.commands,
                error_count: snapshot.header.errors,
                screenshot_count: snapshot.header.screenshots,
                skipped_assets: 0,
                capture_incomplete: captured.partial,
            }
        };
        let standalone = archive.is_none();
        let mut screenshots = if standalone {
            html::Screenshots::Inline(HashMap::new())
        } else {
            html::Screenshots::Relative(HashSet::new())
        };
        if options.include_screenshots {
            let references = screenshot_references(
                &nodes,
                selected,
                if standalone {
                    BTreeSet::new()
                } else {
                    captured.screenshot_refs
                },
                &mut stats,
                cancelled,
            )?;
            let journal = Directory::open(&snapshot.directory, false)?;
            #[cfg(unix)]
            let same = journal.same_as_file(source.parent_file())?;
            #[cfg(windows)]
            let same = journal.same_as_directory(source.parent_directory())?;
            if !same {
                return Err(super::changed());
            }
            let mut inline_bytes = 0;
            write_screenshots(
                &journal,
                references,
                filtered || standalone,
                &mut stats,
                &mut |path, bytes| {
                    match &mut screenshots {
                        html::Screenshots::Relative(paths) => {
                            let emit = archive.as_mut().expect("archive renderer");
                            emit(path, bytes)?;
                            paths.insert(path.to_owned());
                        }
                        html::Screenshots::Inline(urls) => {
                            if inline_bytes + bytes.len() > MAX_INLINE_PNG_BYTES {
                                return Ok(false);
                            }
                            inline_bytes += bytes.len();
                            let mut url = String::from("data:image/png;base64,");
                            STANDARD.encode_string(bytes, &mut url);
                            urls.insert(path.to_owned(), url);
                        }
                    }
                    Ok(true)
                },
                cancelled,
            )?;
        }
        if let Some(emit) = &mut archive {
            let published_xml =
                xml::rewrite(original, &document, &nodes, selected, options, &stats)?;
            check_cancelled(cancelled)?;
            emit(DOCUMENT, published_xml.as_bytes())?;
        }
        let page = html::render(
            snapshot,
            selected,
            options,
            &log,
            &screenshots,
            links_allowed,
            &stats,
        )?;
        check_cancelled(cancelled)?;
        if let Some(emit) = archive {
            emit("journal.html", page.as_bytes())?;
        }
        snapshot.identity.require_document(source.file())?;
        snapshot.open_current_source()?;
        check_cancelled(cancelled)?;
        Ok((page, stats))
    }
}

fn count_errors(nodes: &[Node<'_, '_>], selected: &[&Entry]) -> io::Result<u64> {
    selected.iter().try_fold(0, |count, entry| {
        let node = nodes[entry.xml_index];
        Ok(count + u64::from(bounded_field(node, "marker")? == "ERROR"))
    })
}

fn screenshot_references(
    nodes: &[Node<'_, '_>],
    selected: &[&Entry],
    mut references: BTreeSet<capture::ScreenshotRef>,
    stats: &mut BundleStats,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<BTreeSet<capture::ScreenshotRef>> {
    if references.len() > MAX_SCREENSHOT_REFERENCES {
        return Err(invalid(
            "The journal bundle has too many screenshot references",
        ));
    }
    for entry in selected {
        check_cancelled(cancelled)?;
        let reference = bounded_field(nodes[entry.xml_index], "screenshotFile")?;
        if reference.is_empty() {
            continue;
        }
        if let Some(reference) = capture::ScreenshotRef::parse(&reference) {
            references.insert(reference);
        } else {
            stats.skipped_assets += 1;
        }
        if references.len() > MAX_SCREENSHOT_REFERENCES {
            return Err(invalid(
                "The journal bundle has too many screenshot references",
            ));
        }
    }
    Ok(references)
}

fn write_screenshots(
    journal: &Directory,
    mut references: BTreeSet<capture::ScreenshotRef>,
    filtered: bool,
    stats: &mut BundleStats,
    emit: &mut impl FnMut(&str, &[u8]) -> io::Result<bool>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    if filtered && references.is_empty() {
        return Ok(());
    }
    let folder = match journal.deletion_child(OsStr::new("screenshots")) {
        Ok(folder) => folder,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            stats.skipped_assets += references.len();
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if !filtered {
        add_directory_screenshots(&folder, &mut references, stats, cancelled)?;
    }
    let mut used = HashSet::new();
    for reference in references {
        check_cancelled(cancelled)?;
        let path = reference.as_str();
        let key = path.to_ascii_lowercase();
        if !used.insert(key) {
            stats.skipped_assets += 1;
            continue;
        }
        let name = path
            .strip_prefix("screenshots/")
            .ok_or_else(|| invalid("Invalid journal screenshot reference"))?;
        let Ok(bytes) = super::markdown::screenshot_bytes(
            || open_published_image(&folder, name),
            MAX_PNG_BYTES,
        ) else {
            stats.skipped_assets += 1;
            continue;
        };
        if !emit(path, &bytes)? {
            stats.skipped_assets += 1;
        }
    }
    if !folder.same_as(&journal.deletion_child(OsStr::new("screenshots"))?)? {
        return Err(invalid(
            "The journal screenshot directory changed during export",
        ));
    }
    Ok(())
}

fn add_directory_screenshots(
    folder: &Directory,
    references: &mut BTreeSet<capture::ScreenshotRef>,
    stats: &mut BundleStats,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    for name in folder.names(MAX_SCREENSHOT_REFERENCES)? {
        check_cancelled(cancelled)?;
        let Some(name) = name.to_str() else { continue };
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".png") || lower.ends_with(".orig.png") {
            continue;
        }
        if let Some(reference) = capture::ScreenshotRef::parse(&format!("screenshots/{name}")) {
            references.insert(reference);
        } else {
            stats.skipped_assets += 1;
        }
        if references.len() > MAX_SCREENSHOT_REFERENCES {
            return Err(invalid(
                "The journal bundle has too many screenshot references",
            ));
        }
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

#[cfg(test)]
mod tests;
