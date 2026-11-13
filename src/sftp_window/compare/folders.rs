//! Reviewed metadata comparison for two bounded SFTP folder manifests.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use globset::{Candidate, GlobBuilder, GlobSet, GlobSetBuilder};
use iced::widget::{self, button, checkbox, column, row, scrollable, text, text_editor};
use iced::{Alignment, Element, Length, Task};
use kortty_ssh::sftp::Sftp;

use crate::sftp_window::tree::snapshot::{self, Entry, Kind, Manifest, Relative};
use crate::sftp_window::{Location, worker};

use super::Message;

const MAX_PATTERNS: usize = 32;
const MAX_PATTERN_BYTES: usize = 256;
const MAX_EXCLUDE_BYTES: usize = MAX_PATTERNS * (MAX_PATTERN_BYTES + 1);
const PAGE_ROWS: usize = 60;
const TIME_TOLERANCE: Duration = Duration::from_secs(2);

pub(crate) struct Request {
    roots: [Location; 2],
    excludes: Excludes,
}

impl Request {
    pub(in crate::sftp_window) fn new(roots: [Location; 2], patterns: &str) -> io::Result<Self> {
        Ok(Self {
            roots,
            excludes: excludes(patterns)?,
        })
    }
}

pub struct Review {
    manifests: [Manifest; 2],
    excludes: Excludes,
    rows: Vec<(Relative, State)>,
    differences: usize,
}

impl fmt::Debug for Review {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("FolderComparison")
            .field("entries", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl Review {
    pub(in crate::sftp_window) fn review_bytes(
        &self,
        mut write: impl FnMut(&[u8]),
    ) -> io::Result<()> {
        write(&serde_json::to_vec(&self.excludes.patterns)?);
        for (index, manifest) in self.manifests.iter().enumerate() {
            write(&[index as u8]);
            manifest.review_bytes(&mut write)?;
        }
        Ok(())
    }

    pub(in crate::sftp_window) fn direction(
        &self,
        to: crate::sftp_window::Side,
    ) -> Option<(&Manifest, &Manifest)> {
        use crate::sftp_window::Side;
        let [left, right] = &self.manifests;
        let (local, remote) = match (&left.root, &right.root) {
            (Location::Local(_), Location::Remote(_)) => (left, right),
            (Location::Remote(_), Location::Local(_)) => (right, left),
            _ => return None,
        };
        if contains_private_stage(&local.root) || contains_private_stage(&remote.root) {
            return None;
        }
        Some(match to {
            Side::Local => (remote, local),
            Side::Remote => (local, remote),
        })
    }

    pub(in crate::sftp_window) fn source_only(
        &self,
        to: crate::sftp_window::Side,
    ) -> impl Iterator<Item = &Relative> {
        self.only_on(self.direction(to).map(|(source, _)| source))
    }

    pub(in crate::sftp_window) fn destination_only(
        &self,
        to: crate::sftp_window::Side,
    ) -> impl Iterator<Item = &Relative> {
        self.only_on(self.direction(to).map(|(_, destination)| destination))
    }

    pub(in crate::sftp_window) fn replacement_candidates(
        &self,
        to: crate::sftp_window::Side,
    ) -> impl Iterator<Item = &Relative> {
        let direction = self.direction(to).map(|(source, target)| {
            let newer = if std::ptr::eq(source, &self.manifests[0]) {
                State::LeftNewer
            } else {
                State::RightNewer
            };
            (source, target, newer)
        });
        self.rows.iter().filter_map(move |(path, state)| {
            let (source, target, newer) = direction?;
            ((*state == newer || *state == State::DifferentSize)
                && !contains_private_stage_component(path.bytes())
                && source
                    .entries
                    .get(path)
                    .is_some_and(|entry| entry.kind() == Kind::File)
                && target
                    .entries
                    .get(path)
                    .is_some_and(|entry| entry.kind() == Kind::File))
            .then_some(path)
        })
    }

    #[cfg(windows)]
    pub(in crate::sftp_window) fn paired_regular_files(&self) -> impl Iterator<Item = &Relative> {
        let manifests = self.direction(crate::sftp_window::Side::Local);
        self.rows.iter().filter_map(move |(path, _)| {
            let (source, target) = manifests?;
            (!contains_private_stage_component(path.bytes())
                && source
                    .entries
                    .get(path)
                    .is_some_and(|entry| entry.kind() == Kind::File)
                && target
                    .entries
                    .get(path)
                    .is_some_and(|entry| entry.kind() == Kind::File))
            .then_some(path)
        })
    }

    fn only_on(&self, manifest: Option<&Manifest>) -> impl Iterator<Item = &Relative> {
        let wanted = manifest.map(|manifest| {
            if std::ptr::eq(manifest, &self.manifests[0]) {
                State::LeftOnly
            } else {
                State::RightOnly
            }
        });
        self.rows.iter().filter_map(move |(path, state)| {
            (Some(*state) == wanted && !contains_private_stage_component(path.bytes()))
                .then_some(path)
        })
    }

    pub(in crate::sftp_window) fn verify_current(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), worker::Failure> {
        let roots = std::array::from_fn(|index| self.manifests[index].root.clone());
        let current =
            snapshot::read_pair(client, roots, |path| self.excludes.matches(path), cancelled)?;
        if self
            .manifests
            .iter()
            .zip(&current)
            .all(|(before, now)| before.matches(now))
        {
            Ok(())
        } else {
            Err(io::Error::other("A compared folder changed. Compare again before copying.").into())
        }
    }
}

fn contains_private_stage(root: &Location) -> bool {
    match root {
        Location::Local(path) => path
            .iter()
            .any(|part| is_private_stage_component(part.as_encoded_bytes())),
        Location::Remote(path) => contains_private_stage_component(path),
    }
}

fn contains_private_stage_component(path: &[u8]) -> bool {
    // SFTP paths and both sides' comparison keys always use slash delimiters.
    path.split(|&byte| byte == b'/')
        .any(is_private_stage_component)
}

fn is_private_stage_component(name: &[u8]) -> bool {
    fn matches(name: &[u8], prefix: &[u8], suffix: &[u8]) -> bool {
        let id_end = prefix.len() + 32;
        name.len() == id_end + suffix.len()
            && name[..prefix.len()].eq_ignore_ascii_case(prefix)
            && name[prefix.len()..id_end].iter().all(u8::is_ascii_hexdigit)
            && name[id_end..].eq_ignore_ascii_case(suffix)
    }

    matches(name, b".kortty-", b".kortty-part") || matches(name, b".kortty-extract.", b"")
}

pub(in crate::sftp_window) fn read(
    client: &mut Sftp<impl Read, impl Write>,
    request: Request,
    cancelled: impl Fn() -> bool,
) -> Result<Review, worker::Failure> {
    let Request { roots, excludes } = request;
    let manifests = snapshot::read_pair(client, roots, |path| excludes.matches(path), cancelled)?;
    let rows = classify(&manifests[0].entries, &manifests[1].entries);
    let differences = rows
        .iter()
        .filter(|(_, state)| *state != State::Same)
        .count();
    Ok(Review {
        manifests,
        excludes,
        rows,
        differences,
    })
}

pub(in crate::sftp_window) struct Excludes {
    patterns: Vec<String>,
    compiled: GlobSet,
}

impl Excludes {
    pub(in crate::sftp_window) fn matches(&self, path: &[u8]) -> bool {
        self.compiled
            .is_match_candidate(&Candidate::from_bytes(path))
    }
}

pub(in crate::sftp_window) fn excludes(raw: &str) -> io::Result<Excludes> {
    if raw.len() > MAX_EXCLUDE_BYTES {
        return Err(io::Error::other("Folder excludes exceed the text limit"));
    }
    let mut set = GlobSetBuilder::new();
    let mut patterns = Vec::new();
    for pattern in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if patterns.len() == MAX_PATTERNS || pattern.len() > MAX_PATTERN_BYTES {
            return Err(io::Error::other(
                "Use up to 32 excludes of at most 256 bytes each",
            ));
        }
        if pattern.contains(['\\', '\0'])
            || pattern
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(io::Error::other(
                "Exclude globs must use relative paths and forward slashes",
            ));
        }
        let glob = |pattern: &str| {
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|error| io::Error::other(format!("Invalid exclude glob ({error})")))
        };
        set.add(glob(pattern)?);
        if !pattern.contains('/') {
            set.add(glob(&format!("**/{pattern}"))?);
        }
        patterns.push(pattern.to_owned());
    }
    Ok(Excludes {
        patterns,
        compiled: set.build().map_err(io::Error::other)?,
    })
}

pub(in crate::sftp_window) fn validate_excludes(raw: &str) -> io::Result<()> {
    excludes(raw).map(|_| ())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Same,
    LeftOnly,
    RightOnly,
    LeftNewer,
    RightNewer,
    DifferentSize,
    TypeConflict,
    Blocked,
    BlockedByParent,
    ContentsDiffer,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Same => "Metadata matches",
            Self::LeftOnly => "Left only",
            Self::RightOnly => "Right only",
            Self::LeftNewer => "Left is newer",
            Self::RightNewer => "Right is newer",
            Self::DifferentSize => "Different size",
            Self::TypeConflict => "Type conflict",
            Self::Blocked => "Unverified or unsupported",
            Self::BlockedByParent => "Blocked by parent conflict",
            Self::ContentsDiffer => "Contains differences",
        }
    }

    fn blocks_children(self) -> bool {
        matches!(
            self,
            Self::TypeConflict | Self::Blocked | Self::BlockedByParent
        )
    }
}

fn classify(
    left: &BTreeMap<Relative, Entry>,
    right: &BTreeMap<Relative, Entry>,
) -> Vec<(Relative, State)> {
    let paths: BTreeSet<_> = left.keys().chain(right.keys()).collect();
    let mut states = BTreeMap::new();
    for &path in &paths {
        let blocked = path.parent().is_some_and(|parent| {
            states
                .get(&parent)
                .is_some_and(|state: &State| state.blocks_children())
        });
        let state = if blocked {
            State::BlockedByParent
        } else {
            state(left.get(path), right.get(path))
        };
        states.insert((*path).clone(), state);
    }
    for &path in paths.iter().rev() {
        if states[path] != State::Same
            && let Some(parent) = path.parent()
            && let Some(state) = states.get_mut(&parent)
            && *state == State::Same
        {
            *state = State::ContentsDiffer;
        }
    }
    states.into_iter().collect()
}

fn state(left: Option<&Entry>, right: Option<&Entry>) -> State {
    if [left, right].into_iter().flatten().any(|entry| {
        entry.kind() == Kind::Blocked
            || entry.kind() == Kind::File && entry.file_metadata().is_none()
    }) {
        return State::Blocked;
    }
    let (left, right) = match (left, right) {
        (Some(left), Some(right)) => (left, right),
        (Some(_), None) => return State::LeftOnly,
        (None, Some(_)) => return State::RightOnly,
        (None, None) => return State::Same,
    };
    if left.kind() != right.kind() {
        return State::TypeConflict;
    }
    if left.kind() == Kind::Folder {
        return State::Same;
    }
    let (Some((left_size, left_time)), Some((right_size, right_time))) =
        (left.file_metadata(), right.file_metadata())
    else {
        return State::Blocked;
    };
    let difference = left_time
        .duration_since(right_time)
        .unwrap_or_else(|error| error.duration());
    if difference < TIME_TOLERANCE {
        if left_size == right_size {
            State::Same
        } else {
            State::DifferentSize
        }
    } else if left_time > right_time {
        State::LeftNewer
    } else {
        State::RightNewer
    }
}

pub(in crate::sftp_window) struct Setup {
    ticket: u64,
    roots: [Location; 2],
    input: text_editor::Content,
    error: Option<String>,
}

impl Setup {
    pub(super) fn new(ticket: u64, roots: [Location; 2]) -> Self {
        Self {
            ticket,
            roots,
            input: text_editor::Content::new(),
            error: None,
        }
    }

    pub(super) fn edit(&mut self, ticket: u64, action: text_editor::Action) {
        if ticket != self.ticket {
            return;
        }
        let before = self.input.text();
        if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > MAX_EXCLUDE_BYTES)
        {
            self.error = Some("Folder excludes exceed the text limit".into());
            return;
        }
        self.input.perform(action);
        if self.input.text().len() > MAX_EXCLUDE_BYTES {
            self.input = text_editor::Content::with_text(&before);
            self.error = Some("Folder excludes exceed the text limit".into());
        } else {
            self.error = None;
        }
    }

    pub(super) fn start(&mut self, ticket: u64) -> Option<Request> {
        if ticket != self.ticket {
            return None;
        }
        match excludes(&self.input.text()) {
            Ok(excludes) => Some(Request {
                roots: self.roots.clone(),
                excludes,
            }),
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let ticket = self.ticket;
        let mut setup = column![
            text("Review folders to compare").size(19),
            text(root_label("Left", &self.roots[0])),
            text(root_label("Right", &self.roots[1])),
            text("Exclude patterns"),
            text_editor(&self.input)
                .placeholder("One optional glob per line")
                .height(105)
                .on_action(move |action| Message::FolderExcludes(ticket, action)),
            text("A name pattern applies at every depth. Patterns with slashes are relative to each folder."),
        ].spacing(8);
        if let Some(error) = &self.error {
            setup = setup.push(text(error));
        }
        setup
            .push(
                row![
                    button("Compare").on_press(Message::StartFolders(ticket)),
                    button("Cancel").on_press(Message::Close),
                ]
                .spacing(8),
            )
            .padding(14)
            .into()
    }
}

pub(in crate::sftp_window) struct View {
    review: Arc<Review>,
    page: usize,
    differences_only: bool,
    scroll: widget::Id,
    copy_notice: Option<String>,
}

impl View {
    pub(super) fn new(review: Arc<Review>) -> Self {
        Self {
            review,
            page: 0,
            differences_only: false,
            scroll: widget::Id::unique(),
            copy_notice: None,
        }
    }

    pub(super) fn review(&self) -> &Arc<Review> {
        &self.review
    }

    pub(super) fn copy_notice(&mut self, notice: String) {
        self.copy_notice = Some(notice);
    }

    fn pages(&self) -> usize {
        let entries = if self.differences_only {
            self.review.differences
        } else {
            self.review.rows.len()
        };
        entries.div_ceil(PAGE_ROWS).max(1)
    }

    pub(super) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::FolderPage(page) if page < self.pages() => self.page = page,
            Message::FolderDifferencesOnly(value) => {
                self.differences_only = value;
                self.page = 0;
            }
            _ => return Task::none(),
        }
        widget::operation::scroll_to(
            self.scroll.clone(),
            widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
        )
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let pages = self.pages();
        let rows = self
            .review
            .rows
            .iter()
            .filter(|(_, state)| !self.differences_only || *state != State::Same)
            .skip(self.page * PAGE_ROWS)
            .take(PAGE_ROWS)
            .fold(column![].spacing(3), |rows, (path, state)| {
                rows.push(
                    row![
                        text(path.display()).width(Length::FillPortion(3)),
                        text(state.label()).width(Length::FillPortion(2)),
                    ]
                    .spacing(8),
                )
            });
        let mut content = column![
            text("Compare folders").size(19),
            text(root_label("Left", &self.review.manifests[0].root)),
            text(root_label("Right", &self.review.manifests[1].root)),
            text(format!(
                "{} paths · {} differences · {} exclude patterns · comparison by metadata",
                self.review.rows.len(),
                self.review.differences,
                self.review.excludes.patterns.len()
            )),
            checkbox(self.differences_only)
                .label("Differences only")
                .on_toggle(Message::FolderDifferencesOnly),
            scrollable(rows)
                .id(self.scroll.clone())
                .height(Length::Fill),
            row![
                button("Previous")
                    .on_press_maybe(self.page.checked_sub(1).map(Message::FolderPage)),
                text(format!("{} / {pages}", self.page + 1)),
                button("Next").on_press_maybe(
                    (self.page + 1 < pages).then_some(Message::FolderPage(self.page + 1))
                ),
                button("Close").on_press(Message::Close),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        ];
        if self
            .review
            .direction(crate::sftp_window::Side::Local)
            .is_some()
        {
            content = content.push(
                row![
                    button("Review sync to local…")
                        .on_press(Message::ReviewCopy(crate::sftp_window::Side::Local)),
                    button("Review sync to server…")
                        .on_press(Message::ReviewCopy(crate::sftp_window::Side::Remote)),
                ]
                .spacing(8),
            );
        }
        if let Some(notice) = &self.copy_notice {
            content = content.push(text(notice));
        }
        content.height(Length::Fill).padding(14).spacing(8).into()
    }
}

fn root_label(side: &str, root: &Location) -> String {
    let place = match root {
        Location::Local(_) => "Local",
        Location::Remote(_) => "Server",
    };
    format!("{side} · {place} · {}", root.display())
}

#[cfg(test)]
mod tests;
