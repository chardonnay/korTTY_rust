use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length};

use super::{Message, display_name};

const DIRECTORY_LIMIT: usize = 4096;

#[derive(Clone, Debug)]
pub struct Listing {
    pub(super) directory: PathBuf,
    folders: Vec<PathBuf>,
}

impl Listing {
    pub(super) fn contains(&self, path: &Path) -> bool {
        self.directory.parent() == Some(path) || self.folders.iter().any(|folder| folder == path)
    }

    pub(super) fn view<'a>(&'a self, busy: bool, notice: Option<&'a str>) -> Element<'a, Message> {
        let folders = column(self.folders.iter().map(|folder| {
            button(text(display_name(folder)))
                .width(Length::Fill)
                .style(button::text)
                .on_press_maybe((!busy).then(|| Message::BrowseTo(folder.clone())))
                .into()
        }));
        let mut body = column![
            text("Choose recording folder").size(22),
            row![
                button("Up").on_press_maybe(
                    self.directory
                        .parent()
                        .filter(|_| !busy)
                        .map(|path| Message::BrowseTo(path.into()))
                ),
                text(self.directory.to_string_lossy()).size(13),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            container(scrollable(folders).height(Length::Fill))
                .height(Length::Fill)
                .width(Length::Fill)
                .style(container::bordered_box),
        ]
        .spacing(8);
        if let Some(notice) = notice {
            body = body.push(text(notice).size(13));
        }
        container(
            body.push(
                row![
                    space::horizontal(),
                    button("Cancel").on_press_maybe((!busy).then_some(Message::CancelFolder)),
                    button("Choose folder").on_press_maybe((!busy).then_some(Message::UseFolder)),
                ]
                .spacing(8),
            ),
        )
        .padding(15)
        .height(Length::Fill)
        .into()
    }
}

pub(super) fn list(path: &Path, climb: bool) -> io::Result<Listing> {
    let (directory, entries) = open(path, climb)?;
    let mut folders = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index == DIRECTORY_LIMIT {
            return Err(io::Error::other("The folder has more than 4096 entries"));
        }
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name().to_str().is_some() {
            folders.push(entry.path());
        }
    }
    folders.sort_unstable();
    Ok(Listing {
        directory: directory.into(),
        folders,
    })
}

fn open(path: &Path, climb: bool) -> io::Result<(&Path, fs::ReadDir)> {
    for candidate in path.ancestors() {
        match fs::read_dir(candidate) {
            Ok(entries) => return Ok((candidate, entries)),
            Err(error) if climb && error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No recording folder was found",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording_window::tests::TestDirectory;

    #[test]
    fn folder_browser_starts_from_an_existing_ancestor_and_lists_only_directories() {
        let scratch = TestDirectory::new();
        let child = scratch.0.join("capture");
        fs::create_dir(&child).unwrap();
        fs::write(scratch.0.join("file"), b"file").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&child, scratch.0.join("alias")).unwrap();
        let listing = list(&scratch.0.join("missing/replay"), true).unwrap();
        assert_eq!(listing.directory, scratch.0);
        assert_eq!(listing.folders.as_slice(), std::slice::from_ref(&child));
        assert!(listing.contains(&child));
        assert!(!listing.contains(&scratch.0.join("file")));
        assert!(list(&scratch.0.join("missing"), false).is_err());
    }
}
