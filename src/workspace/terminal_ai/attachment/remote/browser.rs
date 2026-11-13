//! Read-only browsing on the same authenticated SSH source as the attachment.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};
use kortty_ssh::sftp::{Attributes, Error as SftpError, FileKind, Sftp};

use crate::terminal_logging::Redactor;

use super::{Access, Cancel, MAX_BYTES, Pending, ReadFailure, Source, absolute_path, start};
use crate::workspace::terminal_ai::mask;

const MAX_ENTRIES: usize = 512;
const MAX_LIST_BYTES: usize = 192 * 1024;

#[derive(Clone)]
pub enum Message {
    Start,
    Listed(u64, Result<Arc<Listing>, String>),
    Parent(u64),
    Choose(u64, usize),
    Back,
}

pub(in crate::workspace::terminal_ai) enum State {
    Loading { ticket: u64, _cancel: Cancel },
    Listed(u64, Arc<Listing>),
}

pub struct Listing {
    source: Source,
    directory: Directory,
    label: String,
    labels: Vec<String>,
}

struct Directory {
    path: String,
    entries: Vec<Entry>,
}

struct Entry {
    name: String,
    attributes: Attributes,
}

impl Entry {
    fn folder(&self) -> bool {
        self.attributes.kind() == FileKind::Directory
    }
}

pub(in crate::workspace::terminal_ai) enum Choice {
    Folder(String),
    File(Box<Selection>),
}

pub(in crate::workspace::terminal_ai) struct Selection {
    pub(in crate::workspace::terminal_ai) path: String,
    source: Source,
    attributes: Attributes,
}

impl Selection {
    pub(super) fn expected(&self, access: &Access, path: &str) -> Result<Attributes, String> {
        if self.source != access.source || self.path != path || access.opener.is_closed() {
            return Err("The original SSH file selection changed. Browse again.".into());
        }
        Ok(self.attributes)
    }
}

impl Access {
    pub(in crate::workspace::terminal_ai) fn start_browse(
        self,
        requested: String,
        redactor: Arc<Redactor>,
    ) -> Result<Pending<Arc<Listing>>, String> {
        start(move |control| {
            let directory = self
                .with_client(control, |client| list(client, &requested))
                .map_err(|error| match error {
                    ReadFailure::TooLarge => {
                        "The folder exceeds the browser limit. Enter an absolute file path instead."
                    }
                    ReadFailure::Unsupported => {
                        "Choose a readable SFTP folder with a regular path."
                    }
                    _ => error.message(),
                })?;
            let label = mask::prompt(&redactor, &directory.path)?;
            let labels = directory
                .entries
                .iter()
                .map(|entry| mask::prompt(&redactor, &entry.name))
                .collect::<Result<_, _>>()?;
            Ok(Arc::new(Listing {
                source: self.source,
                directory,
                label,
                labels,
            }))
        })
    }
}

impl Listing {
    pub(in crate::workspace::terminal_ai) fn matches(&self, access: Option<&Access>) -> bool {
        access.is_some_and(|access| self.source == access.source && !access.opener.is_closed())
    }

    pub(in crate::workspace::terminal_ai) fn parent(&self) -> Option<String> {
        (self.directory.path != "/").then(|| {
            let slash = self
                .directory
                .path
                .rfind('/')
                .expect("absolute SFTP directory");
            self.directory.path[..slash.max(1)].to_owned()
        })
    }

    pub(in crate::workspace::terminal_ai) fn choose(&self, index: usize) -> Option<Choice> {
        let entry = self.directory.entries.get(index)?;
        let path = child(&self.directory.path, &entry.name);
        Some(if entry.folder() {
            Choice::Folder(path)
        } else {
            Choice::File(Box::new(Selection {
                path,
                source: self.source.clone(),
                attributes: entry.attributes,
            }))
        })
    }
}

impl State {
    pub(in crate::workspace::terminal_ai) fn view(&self, allowed: bool) -> Element<'_, Message> {
        match self {
            Self::Loading { .. } => column![
                text("Reading server folders…"),
                button("Back").on_press(Message::Back),
            ]
            .spacing(6)
            .into(),
            Self::Listed(ticket, listing) => {
                let ticket = *ticket;
                let entries = listing.directory.entries.iter().zip(&listing.labels);
                let mut list = column![].spacing(5);
                for (index, (entry, label)) in entries.enumerate() {
                    list = list.push(
                        button(
                            row![
                                text(if entry.folder() { "Folder" } else { "File" }).size(12),
                                text(label).wrapping(text::Wrapping::WordOrGlyph)
                            ]
                            .spacing(8),
                        )
                        .width(Length::Fill)
                        .on_press_maybe(allowed.then_some(Message::Choose(ticket, index))),
                    );
                }
                column![
                    text(&listing.label).wrapping(text::Wrapping::WordOrGlyph),
                    row![
                        button("Up").on_press_maybe(
                            (allowed && listing.directory.path != "/")
                                .then_some(Message::Parent(ticket))
                        ),
                        button("Back").on_press(Message::Back),
                    ]
                    .spacing(8),
                    if listing.labels.is_empty() {
                        text("No eligible folders or text file candidates.").into()
                    } else {
                        Element::from(scrollable(list).height(Length::Fixed(166.0)))
                    },
                ]
                .spacing(6)
                .into()
            }
        }
    }
}

fn list(
    client: &mut Sftp<impl Read, impl Write>,
    requested: &str,
) -> Result<Directory, ReadFailure> {
    if requested != "." && !directory_path(requested) {
        return Err(ReadFailure::InvalidPath);
    }
    let canonical = client.canonicalize(requested)?;
    let path = String::from_utf8(canonical).map_err(|_| ReadFailure::InvalidPath)?;
    if !directory_path(&path) || (requested != "." && path != requested) {
        return Err(ReadFailure::Changed);
    }
    let mut before = client.lstat(&path)?;
    if before.kind() != FileKind::Directory {
        return Err(ReadFailure::Unsupported);
    }
    let names = client
        .read_dir_limited(&path, MAX_ENTRIES, MAX_LIST_BYTES)
        .map_err(|error| {
            if matches!(error, SftpError::Limit(_)) {
                ReadFailure::TooLarge
            } else {
                error.into()
            }
        })?;
    let mut after = client.lstat(&path)?;
    before.accessed = None;
    after.accessed = None;
    if before != after || client.canonicalize(requested)? != path.as_bytes() {
        return Err(ReadFailure::Changed);
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for entry in names {
        let Ok(name) = String::from_utf8(entry.name) else {
            continue;
        };
        if absolute_path(&child(&path, &name)).is_err() {
            continue;
        }
        if !seen.insert(name.clone()) {
            return Err(ReadFailure::Changed);
        }
        let folder = entry.attrs.kind() == FileKind::Directory;
        let file = entry.attrs.kind() == FileKind::File
            && entry.attrs.modified.is_some()
            && entry
                .attrs
                .size
                .is_some_and(|size| size <= MAX_BYTES as u64);
        if folder || file {
            entries.push(Entry {
                name,
                attributes: entry.attrs,
            });
        }
    }
    entries.sort_by(|left, right| {
        right
            .folder()
            .cmp(&left.folder())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(Directory { path, entries })
}

fn directory_path(path: &str) -> bool {
    path == "/" || absolute_path(path).is_ok()
}

fn child(path: &str, name: &str) -> String {
    format!("{}/{name}", path.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};

    use super::super::tests::{attributes, name, packet, status, string, version};
    use super::*;

    #[test]
    fn browser_lists_checked_folders_and_exact_utf8_file_candidates() {
        for after in [b"/reviewed".as_slice(), b"/replaced".as_slice()] {
            let mut wire = version();
            name(&mut wire, 1, b"/reviewed");
            let folder = attributes(0, 0o040700);
            packet(&mut wire, 105, 2, &folder);
            packet(&mut wire, 102, 3, &string(b"dir"));
            let entries = [
                (b"link".as_slice(), attributes(12, 0o120777)),
                ("Grüße.txt".as_bytes(), attributes(32, 0o100600)),
                (b"nested".as_slice(), folder.clone()),
                (
                    b"too-big.txt".as_slice(),
                    attributes(MAX_BYTES as u64 + 1, 0o100600),
                ),
                (b"raw\xff".as_slice(), attributes(12, 0o100600)),
                (b"control\n".as_slice(), attributes(12, 0o100600)),
            ];
            let mut payload = (entries.len() as u32).to_be_bytes().to_vec();
            for (name, attributes) in entries {
                payload.extend(string(name));
                payload.extend(string(b""));
                payload.extend(attributes);
            }
            packet(&mut wire, 104, 4, &payload);
            status(&mut wire, 5, 1);
            status(&mut wire, 6, 0);
            packet(&mut wire, 105, 7, &folder);
            name(&mut wire, 8, after);
            let mut client = Sftp::new(Cursor::new(wire), io::sink()).unwrap();
            let result = list(&mut client, ".");
            if after == b"/reviewed" {
                let directory = result.unwrap();
                assert_eq!(directory.path, "/reviewed");
                assert_eq!(directory.entries.len(), 2);
                assert!(directory.entries[0].folder());
                assert_eq!(directory.entries[0].name, "nested");
                assert_eq!(directory.entries[1].name, "Grüße.txt");
            } else {
                assert_eq!(result.err(), Some(ReadFailure::Changed));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires an explicit SFTP_SERVER OpenSSH test executable"]
    fn openssh_chat_browser_lists_and_loads_a_reviewed_file() {
        use std::fs;
        use std::process::{Child, Command, Stdio};

        struct Server(Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let run = crate::snippets::tests::Profile::new();
        let path = run.0.canonicalize().unwrap();
        fs::create_dir(path.join("nested")).unwrap();
        fs::write(path.join("Grüße.txt"), "File evidence\n").unwrap();
        std::os::unix::fs::symlink("Grüße.txt", path.join("link.txt")).unwrap();
        let mut server = Server(
            Command::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
                .arg("-d")
                .arg(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut client = Sftp::new(
            server.0.stdout.take().unwrap(),
            server.0.stdin.take().unwrap(),
        )
        .unwrap();
        let directory = list(&mut client, ".").unwrap();
        assert_eq!(directory.path, path.to_str().unwrap());
        assert_eq!(directory.entries.len(), 2);
        assert_eq!(directory.entries[0].name, "nested");
        let file = &directory.entries[1];
        assert_eq!(file.name, "Grüße.txt");
        let requested = child(&directory.path, &file.name);
        let read = super::super::read_file(
            &mut client,
            requested.as_bytes(),
            Some(file.attributes),
            MAX_BYTES,
        )
        .unwrap();
        assert_eq!(read.bytes.as_slice(), b"File evidence\n");
        fs::write(path.join("Grüße.txt"), "Different contents\n").unwrap();
        assert_eq!(
            super::super::read_file(
                &mut client,
                requested.as_bytes(),
                Some(file.attributes),
                MAX_BYTES,
            )
            .err(),
            Some(ReadFailure::Changed)
        );
    }
}
