//! Exact breadcrumb targets and bounded, read-only folder measurements.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{button, row, scrollable, text};
use iced::{Alignment, Element, Task};
use kortty_ssh::sftp::{FileKind, Sftp};

use super::tree::snapshot::{self, Entry as SnapshotEntry, Kind};
use super::{
    Directory, EntryKind, EntryName, Location, MAX_SELECTION, Message, SftpWindow, Side, files,
    worker,
};

const MAX_CRUMBS: usize = 128;
pub(in crate::sftp_window) const SCAN_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct Navigation {
    pub(super) source: Arc<Directory>,
    pub(super) target: Location,
}

struct Crumb {
    label: String,
    target: Location,
}

fn crumbs(location: &Location) -> Vec<Crumb> {
    let mut crumbs = match location {
        Location::Local(path) => path
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|path| Crumb {
                label: files::display(
                    &path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy(),
                ),
                target: Location::Local(path.to_path_buf()),
            })
            .collect::<Vec<_>>(),
        Location::Remote(path) => {
            let mut crumbs = Vec::new();
            let root = if path.starts_with(b"/") {
                1
            } else if matches!(path.as_slice(), [drive, b':', b'/', ..] if drive.is_ascii_alphabetic())
            {
                3
            } else {
                path.len()
            };
            if root != 0 {
                crumbs.push(Crumb {
                    label: files::display(&String::from_utf8_lossy(&path[..root])),
                    target: Location::Remote(path[..root].to_vec()),
                });
            }
            let mut begin = root;
            for end in root..=path.len() {
                if end == path.len() || path[end] == b'/' {
                    if end > begin {
                        let part = &path[begin..end];
                        let label = std::str::from_utf8(part)
                            .map(files::display)
                            .unwrap_or_else(|_| part.escape_ascii().to_string());
                        crumbs.push(Crumb {
                            label,
                            target: Location::Remote(path[..end].to_vec()),
                        });
                    }
                    begin = end + 1;
                }
            }
            crumbs
        }
    };
    if crumbs.len() > MAX_CRUMBS {
        crumbs.drain(1..crumbs.len() - MAX_CRUMBS + 1);
        crumbs[1].label.insert_str(0, "… / ");
    }
    crumbs
}

pub(super) fn breadcrumbs_view(
    directory: Option<&Arc<Directory>>,
    available: bool,
) -> Element<'_, Message> {
    let mut links = row![].spacing(2).align_y(Alignment::Center);
    if let Some(directory) = directory {
        for (index, crumb) in crumbs(&directory.path).into_iter().enumerate() {
            if index != 0 {
                links = links.push(text("›").size(12));
            }
            links = links.push(
                button(text(crumb.label).size(12))
                    .style(button::text)
                    .padding([2, 4])
                    .on_press_maybe(available.then(|| {
                        Message::Breadcrumb(Navigation {
                            source: Arc::clone(directory),
                            target: crumb.target,
                        })
                    })),
            );
        }
    }
    scrollable(links)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new(),
        ))
        .into()
}

pub(crate) struct SizeRequest {
    pub(super) source: Arc<Directory>,
    pub(super) selected: Vec<EntryName>,
    pub(super) allocated: Option<worker::folder_usage::Route>,
}

impl SizeRequest {
    pub(super) fn is_allocated(&self) -> bool {
        self.allocated.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Measurement {
    Logical,
    Allocated,
}

pub(super) struct AllocationBatch {
    pub(super) folders: Vec<EntryName>,
    pub(super) bytes: u64,
}

#[derive(Debug)]
pub(super) enum Detail {
    Logical {
        files: usize,
        skipped: usize,
        fallback: bool,
    },
    Allocated {
        partial: bool,
    },
}

#[derive(Debug)]
pub struct Sizes {
    pub(super) source: Arc<Directory>,
    pub(super) requested: Vec<EntryName>,
    pub(super) bytes: HashMap<EntryName, u64>,
    pub(super) total: u64,
    pub(super) detail: Detail,
}

impl SftpWindow {
    pub(super) fn measurement_running(&self) -> bool {
        self.busy.as_ref().is_some_and(|busy| busy.measuring)
    }

    pub(super) fn browse_ancestor(&mut self, navigation: Navigation) -> Task<Message> {
        let side = side(&navigation.source.path);
        if self
            .panel(side)
            .directory
            .as_ref()
            .is_some_and(|directory| {
                Arc::ptr_eq(directory, &navigation.source)
                    && crumbs(&directory.path)
                        .iter()
                        .any(|crumb| crumb.target == navigation.target)
            })
        {
            self.browse(navigation.target)
        } else {
            Task::none()
        }
    }

    pub(super) fn measure_folders(&mut self, side: Side) -> Task<Message> {
        let panel = self.panel(side);
        let Some(directory) = &panel.directory else {
            return Task::none();
        };
        let selected = panel
            .entries()
            .filter(|entry| {
                entry.kind == EntryKind::Directory && panel.selected.contains(&entry.name)
            })
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        if selected.is_empty() || selected.len() > MAX_SELECTION {
            return Task::none();
        }
        let allocated = match side {
            Side::Local => None,
            Side::Remote => self
                .worker
                .as_ref()
                .and_then(worker::folder_usage::Route::new),
        };
        let title = if allocated.is_some() {
            "Calculating server disk usage"
        } else {
            "Calculating logical folder sizes"
        };
        self.dispatch(
            worker::Operation::MeasureFolders(Box::new(SizeRequest {
                source: Arc::clone(directory),
                selected,
                allocated,
            })),
            title,
            None,
        )
    }

    pub(super) fn apply_folder_sizes(&mut self, result: &Sizes) {
        let panel = self.panel_mut(side(&result.source.path));
        if !panel
            .directory
            .as_ref()
            .is_some_and(|directory| Arc::ptr_eq(directory, &result.source))
        {
            return;
        }
        let method = match result.detail {
            Detail::Logical { .. } => Measurement::Logical,
            Detail::Allocated { .. } => Measurement::Allocated,
        };
        if panel.measurement != Some(method) {
            panel.folder_sizes.clear();
            panel.measurement = Some(method);
        }
        panel.allocation_batch = match result.detail {
            Detail::Allocated { partial: false } => Some(AllocationBatch {
                folders: result.requested.clone(),
                bytes: result.total,
            }),
            _ => None,
        };
        for name in &result.requested {
            panel.folder_sizes.remove(name);
        }
        panel.folder_sizes.extend(
            result
                .bytes
                .iter()
                .map(|(name, size)| (name.clone(), *size)),
        );
        panel.reorder();
        self.status = Some(match result.detail {
            Detail::Logical {
                files,
                skipped,
                fallback,
            } => {
                let note = if fallback {
                    " · Server disk usage unavailable"
                } else {
                    ""
                };
                format!(
                    "{} folders · {} logical file bytes · {files} files · {skipped} links or special entries skipped{note}",
                    result.bytes.len(),
                    result.total,
                )
            }
            Detail::Allocated { partial } => {
                let note = if partial {
                    " · Partial server result"
                } else {
                    ""
                };
                format!(
                    "{} of {} folders · {} allocated bytes{note}",
                    result.bytes.len(),
                    result.requested.len(),
                    result.total,
                )
            }
        });
    }
}

fn side(location: &Location) -> Side {
    match location {
        Location::Local(_) => Side::Local,
        Location::Remote(_) => Side::Remote,
    }
}

pub(super) fn measure(
    client: &mut Sftp<impl Read, impl Write>,
    mut request: SizeRequest,
    cancelled: impl Fn() -> bool,
) -> Result<Sizes, worker::Failure> {
    if let Some(route) = request.allocated.take() {
        return worker::folder_usage::measure(client, request, &route);
    }
    measure_logical(client, request, cancelled, false)
}

pub(super) fn measure_logical(
    client: &mut Sftp<impl Read, impl Write>,
    request: SizeRequest,
    cancelled: impl Fn() -> bool,
    fallback: bool,
) -> Result<Sizes, worker::Failure> {
    let started = Instant::now();
    let stop = || cancelled() || started.elapsed() >= SCAN_TIMEOUT;
    let SizeRequest {
        source, selected, ..
    } = request;
    if selected.is_empty() || selected.len() > MAX_SELECTION {
        return Err(io::Error::other("Choose up to 256 folders to measure").into());
    }
    let mut roots = Vec::with_capacity(selected.len());
    for name in &selected {
        if stop() {
            return Err(io::Error::other("Folder size calculation cancelled or timed out").into());
        }
        let entry = source
            .entries
            .iter()
            .find(|entry| &entry.name == name && entry.kind == EntryKind::Directory)
            .ok_or_else(|| io::Error::other("The selected folder changed. Refresh the listing."))?;
        let path = source
            .path
            .with(name)
            .ok_or_else(|| io::Error::other("The selected folder changed"))?;
        if let Location::Remote(path) = &path {
            let current = client.lstat(path)?;
            if current.kind() != FileKind::Directory
                || entry
                    .modified
                    .is_some_and(|value| current.modified != Some(value))
                || entry
                    .mode
                    .is_some_and(|value| current.permissions != Some(value))
            {
                return Err(
                    io::Error::other("The selected folder changed. Refresh the listing.").into(),
                );
            }
        }
        roots.push(path);
    }
    let manifests = snapshot::read_folder_sizes(client, roots.clone(), stop)?;
    // Re-read metadata before publication; no file contents or shell commands are needed.
    let checked = snapshot::read_folder_sizes(client, roots, stop)?;
    if !manifests
        .iter()
        .zip(&checked)
        .all(|(before, after)| before.matches(after))
        || stop()
    {
        return Err(io::Error::other(
            "Folder contents changed or the size calculation stopped. Try again.",
        )
        .into());
    }
    let mut files = 0;
    let mut skipped = 0;
    let mut sizes = HashMap::new();
    let mut total = 0_u64;
    for (name, manifest) in selected.iter().zip(manifests) {
        let mut bytes = 0u64;
        for entry in manifest.entries.values() {
            let length = match entry {
                SnapshotEntry::LocalFile { stamp, .. } => Some(stamp.size),
                SnapshotEntry::Remote(attributes) if attributes.kind() == FileKind::File => Some(
                    attributes
                        .size
                        .ok_or_else(|| io::Error::other("The server omitted a file size"))?,
                ),
                _ => None,
            };
            if let Some(length) = length {
                bytes = bytes
                    .checked_add(length)
                    .ok_or_else(|| io::Error::other("Folder size exceeds the supported range"))?;
                files += 1;
            } else if entry.kind() == Kind::Blocked {
                skipped += 1;
            }
        }
        total = total
            .checked_add(bytes)
            .ok_or_else(|| io::Error::other("Folder size exceeds the supported range"))?;
        sizes.insert(name.clone(), bytes);
    }
    Ok(Sizes {
        source,
        requested: selected,
        bytes: sizes,
        total,
        detail: Detail::Logical {
            files,
            skipped,
            fallback,
        },
    })
}
