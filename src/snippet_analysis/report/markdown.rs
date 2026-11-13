//! Native Markdown diagram companions and no-replace publication.

use std::io::{self, Write as _};
use std::sync::atomic::AtomicBool;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::private_files::Directory;
pub(super) use crate::private_files::batch::Stage;

use super::{CheckedWriter, Item, Packing, check_cancelled, invalid, valid_component};

const LINK_PATH: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub(super) enum Link {
    Image(String),
    Failed(String),
}

pub(super) fn href(name: &str) -> impl std::fmt::Display + '_ {
    utf8_percent_encode(name, LINK_PATH)
}

pub(super) fn image_name(report: &str, number: Option<usize>) -> io::Result<String> {
    let base = report
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("md"))
        .map_or(report, |(stem, _)| stem);
    let name = match number {
        Some(number) => format!("{base}.{number:02}-diagram.png"),
        None => format!("{base}.diagram.png"),
    };
    if !valid_component(&name) {
        return Err(invalid(
            "Select a shorter Markdown export filename for its diagram companion",
        ));
    }
    Ok(name)
}

pub(super) fn prepare<'a>(
    parent: &'a Directory,
    name: &str,
    reports: &[Item],
    packing: Packing,
    remaining: &mut usize,
    cancelled: &AtomicBool,
) -> io::Result<(Option<Stage<'a>>, Vec<Option<Link>>)> {
    let mut assets = None;
    let mut links = Vec::with_capacity(reports.len());
    for (index, item) in reports.iter().enumerate() {
        check_cancelled(cancelled)?;
        let link = match item
            .record
            .diagram
            .as_ref()
            .map(|diagram| diagram.report_png())
        {
            Some(Ok(png)) => {
                let name = image_name(name, (packing == Packing::Combined).then_some(index + 1))?;
                if assets.is_none() {
                    assets = Some(Stage::new(parent, ".kortty-analysis-export")?);
                }
                if parent.has_entry(&name)? {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("The Markdown diagram companion {name} already exists"),
                    ));
                }
                assets
                    .as_mut()
                    .expect("active Markdown stage")
                    .write_file(&name, |file| {
                        CheckedWriter::new(file, remaining, cancelled).write_all(&png)
                    })?;
                Some(Link::Image(name))
            }
            Some(Err(error)) => Some(Link::Failed(error)),
            None => None,
        };
        links.push(link);
    }
    Ok((assets, links))
}
