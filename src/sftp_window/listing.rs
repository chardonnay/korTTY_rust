//! SFTP browser columns and stable ordering within file groups.

use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::time::SystemTime;

use iced::widget::{button, container, row, text, tooltip};
use iced::{Alignment, Element, Length};
use jiff::{Timestamp, tz::TimeZone};
use kortty::ssh_runtime::account_lookup::DisplayNames;

use super::{Directory, Entry, EntryKind, EntryName, Message, Side, local_accounts};

mod type_icon;

pub(super) const MIN_TABLE_WIDTH: f32 = 550.0;
const WIDTHS: [u16; 7] = [6, 3, 3, 7, 3, 3, 4];

pub(super) fn group(entry: &Entry) -> (bool, bool) {
    (!entry.hidden, entry.kind != EntryKind::Directory)
}

#[derive(Clone, Copy)]
pub(super) enum AccountLabels<'a> {
    Local(&'a local_accounts::Labels),
    Remote(&'a DisplayNames),
}

impl<'a> AccountLabels<'a> {
    fn owner(self, entry: &Entry) -> Option<&'a str> {
        match self {
            Self::Local(labels) => labels.owner(entry),
            Self::Remote(labels) => labels.users.get(&entry.owner?).map(String::as_str),
        }
    }

    fn group(self, entry: &Entry) -> Option<&'a str> {
        match self {
            Self::Local(labels) => labels.group(entry),
            Self::Remote(labels) => labels.groups.get(&entry.group?).map(String::as_str),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    #[default]
    Type,
    Size,
    Date,
    Owner,
    Group,
    Permissions,
}

const COLUMNS: [(&str, SortColumn); 7] = [
    ("Name", SortColumn::Name),
    ("Type", SortColumn::Type),
    ("Size", SortColumn::Size),
    ("Date", SortColumn::Date),
    ("User", SortColumn::Owner),
    ("Group", SortColumn::Group),
    ("Permissions", SortColumn::Permissions),
];

#[derive(Default)]
pub(super) struct Sort {
    column: SortColumn,
    descending: bool,
}

impl Sort {
    pub(super) fn select(&mut self, column: SortColumn) {
        self.descending = column == self.column && !self.descending;
        self.column = column;
    }

    pub(super) fn by_account(&self) -> bool {
        matches!(self.column, SortColumn::Owner | SortColumn::Group)
    }

    pub(super) fn order(
        &self,
        directory: &Directory,
        folder_sizes: &HashMap<EntryName, u64>,
        names: AccountLabels<'_>,
    ) -> Vec<usize> {
        let entries = &directory.entries;
        if matches!(self.column, SortColumn::Name | SortColumn::Type) {
            if !self.descending {
                return Vec::new();
            }
            let mut order: Vec<_> = (0..entries.len()).collect();
            order.sort_by_cached_key(|&index| {
                let entry = &entries[index];
                (group(entry), Reverse(entry.display.to_lowercase()))
            });
            return order;
        }
        let size = |entry: &Entry| folder_sizes.get(&entry.name).copied().or(entry.size);
        let mode = |entry: &Entry| entry.mode.map(|mode| mode & 0o777);
        let dates: Vec<_> = if self.column == SortColumn::Date {
            entries
                .iter()
                .map(|entry| directory.modified(entry))
                .collect()
        } else {
            Vec::new()
        };
        let mut order: Vec<_> = (0..entries.len()).collect();
        order.sort_unstable_by(|&left, &right| {
            let a = &entries[left];
            let b = &entries[right];
            group(a)
                .cmp(&group(b))
                .then_with(|| match self.column {
                    SortColumn::Size if self.descending => size(b).cmp(&size(a)),
                    SortColumn::Size => size(a).cmp(&size(b)),
                    SortColumn::Date => known_first(dates[left], dates[right], self.descending),
                    SortColumn::Owner => principal_order(
                        (a.owner, names.owner(a)),
                        (b.owner, names.owner(b)),
                        self.descending,
                    ),
                    SortColumn::Group => principal_order(
                        (a.group, names.group(a)),
                        (b.group, names.group(b)),
                        self.descending,
                    ),
                    SortColumn::Permissions => known_first(mode(a), mode(b), self.descending),
                    SortColumn::Name | SortColumn::Type => Ordering::Equal,
                })
                .then_with(|| left.cmp(&right))
        });
        order
    }

    pub(super) fn header(&self, side: Side, enabled: bool) -> Element<'_, Message> {
        row(COLUMNS
            .into_iter()
            .zip(WIDTHS)
            .map(|((title, column), width)| {
                let arrow = if self.column == column {
                    if self.descending { " ↓" } else { " ↑" }
                } else {
                    ""
                };
                button(
                    text(format!("{title}{arrow}"))
                        .size(12)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .width(Length::FillPortion(width))
                .padding([2, 3])
                .on_press_maybe(enabled.then_some(Message::Sort(side, column)))
                .into()
            }))
        .align_y(Alignment::Center)
        .spacing(6)
        .into()
    }
}

fn known_first<T: Ord>(left: Option<T>, right: Option<T>, descending: bool) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) if descending => right.cmp(&left),
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn principal_order(
    (left_id, left_name): (Option<u32>, Option<&str>),
    (right_id, right_name): (Option<u32>, Option<&str>),
    descending: bool,
) -> Ordering {
    let order = match (left_name, right_name) {
        (Some(left), Some(right)) => left
            .chars()
            .flat_map(char::to_lowercase)
            .cmp(right.chars().flat_map(char::to_lowercase)),
        (Some(_), None) if right_id.is_none() => return Ordering::Less,
        (None, Some(_)) if left_id.is_none() => return Ordering::Greater,
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => return known_first(left_id, right_id, descending),
    };
    if descending { order.reverse() } else { order }
}

pub(super) fn columns<'a>(
    entry: &'a Entry,
    measured: Option<u64>,
    allocated: bool,
    modified: Option<SystemTime>,
    zone: &TimeZone,
    names: AccountLabels<'a>,
) -> Element<'a, Message> {
    let kind = match entry.kind {
        EntryKind::Directory => "Folder",
        EntryKind::File => "File",
        EntryKind::Link => "Link",
        EntryKind::Other => "Other",
    };
    let mut size = measured
        .or(entry.size)
        .map_or_else(|| "—".into(), size_label);
    if measured.is_some() && allocated {
        size.push_str(" on disk");
    }
    let values: [Cow<'a, str>; 7] = [
        entry.display.as_str().into(),
        kind.into(),
        size.into(),
        date_label(modified, zone).into(),
        principal_label(entry.owner, names.owner(entry)),
        principal_label(entry.group, names.group(entry)),
        permissions_label(entry.mode).into(),
    ];
    row(values
        .into_iter()
        .zip(WIDTHS)
        .enumerate()
        .map(|(index, (label, width))| {
            if index == 1 {
                return container(tooltip(
                    type_icon::widget(entry.kind, &entry.display, entry.hidden),
                    text(label).size(12),
                    tooltip::Position::Top,
                ))
                .width(Length::FillPortion(width))
                .align_x(Alignment::Center)
                .into();
            }
            text(label)
                .size(if index == 0 { 14 } else { 12 })
                .width(Length::FillPortion(width))
                .align_x(if index == 2 {
                    Alignment::End
                } else {
                    Alignment::Start
                })
                .wrapping(text::Wrapping::WordOrGlyph)
                .into()
        }))
    .align_y(Alignment::Center)
    .spacing(6)
    .into()
}

fn principal_label(id: Option<u32>, name: Option<&str>) -> Cow<'_, str> {
    match (id, name) {
        (_, Some(name)) => Cow::Borrowed(name),
        (Some(id), None) => Cow::Owned(id.to_string()),
        (None, None) => Cow::Borrowed("—"),
    }
}

fn size_label(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let exponent = (bytes.ilog2() / 10).min(6);
    let unit = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"][exponent as usize];
    format!(
        "{:.1} {unit}",
        bytes as f64 / (1u64 << (exponent * 10)) as f64
    )
}

fn date_label(modified: Option<SystemTime>, zone: &TimeZone) -> String {
    modified
        .and_then(|modified| Timestamp::try_from(modified).ok())
        .map_or_else(
            || "—".into(),
            |timestamp| {
                timestamp
                    .to_zoned(zone.clone())
                    .strftime("%F %T")
                    .to_string()
            },
        )
}

fn permissions_label(mode: Option<u32>) -> String {
    let Some(mode) = mode else {
        return "—".into();
    };
    "rwxrwxrwx"
        .chars()
        .enumerate()
        .map(|(index, letter)| {
            if mode & (0o400 >> index) != 0 {
                letter
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::sftp_window::{Location, Panel, files};

    fn remote(name: &str, kind: EntryKind, metadata: Option<(u32, u32, u32)>) -> Entry {
        Entry {
            name: EntryName::Remote(name.as_bytes().to_vec()),
            display: name.into(),
            hidden: name.starts_with('.'),
            kind,
            size: (kind == EntryKind::File).then_some(4096),
            modified: metadata.map(|values| values.0),
            owner: metadata.map(|values| values.1),
            group: None,
            mode: metadata.map(|values| values.2),
        }
    }

    fn labels(panel: &Panel) -> Vec<&str> {
        panel
            .entries()
            .map(|entry| entry.display.as_str())
            .collect()
    }

    #[test]
    fn sftp_listing_sorts_remote_metadata_inside_fixed_groups_and_keeps_selection() {
        use EntryKind::{Directory as Folder, File};
        let directory = Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            vec![
                remote("unknown", File, None),
                remote("a", File, Some((60, 3, 0o100640))),
                remote("beta", File, Some((30, 7, 0o100600))),
                remote(".hidden-file", File, Some((90, 9, 0o100755))),
                remote("public", Folder, Some((20, 8, 0o040755))),
                remote(".hidden-folder", Folder, Some((100, 10, 0o040755))),
            ],
        );
        let mut panel = Panel::default();
        panel.load(directory.into());
        assert_eq!(panel.sort.column, SortColumn::Type);
        let ascending = [
            ".hidden-folder",
            ".hidden-file",
            "public",
            "a",
            "beta",
            "unknown",
        ];
        assert_eq!(labels(&panel), ascending);

        panel.sort(SortColumn::Type);
        assert_eq!(
            labels(&panel),
            [
                ".hidden-folder",
                ".hidden-file",
                "public",
                "unknown",
                "beta",
                "a"
            ]
        );
        panel.sort(SortColumn::Name);
        assert_eq!(labels(&panel), ascending);
        panel.sort(SortColumn::Name);
        assert_eq!(labels(&panel)[3..], ["unknown", "beta", "a"]);

        let selected = EntryName::Remote(b"a".to_vec());
        panel.toggle(selected.clone(), true);
        for column in [SortColumn::Date, SortColumn::Permissions] {
            panel.sort(column);
            assert_eq!(
                labels(&panel),
                [
                    ".hidden-folder",
                    ".hidden-file",
                    "public",
                    "beta",
                    "a",
                    "unknown"
                ]
            );
            panel.sort(column);
            assert_eq!(labels(&panel), ascending);
            assert_eq!(panel.selected().unwrap().1.name, selected);
        }
        panel.sort(SortColumn::Owner);
        assert_eq!(labels(&panel), ascending);
        panel.sort(SortColumn::Owner);
        assert_eq!(
            labels(&panel),
            [
                ".hidden-folder",
                ".hidden-file",
                "public",
                "beta",
                "a",
                "unknown"
            ]
        );
        assert_eq!(permissions_label(Some(0o100640)), "rw-r-----");
        assert_eq!(permissions_label(None), "—");
        let directory = panel.directory.as_ref().unwrap();
        let entry = panel.selected().unwrap().1;
        assert_eq!(entry.modified, Some(60));
        assert_eq!(
            date_label(directory.modified(entry), &TimeZone::UTC),
            "1970-01-01 00:01:00"
        );

        let mut sizes = Panel::default();
        sizes.load(
            Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                vec![
                    remote("a-measured", Folder, None),
                    remote("b-unmeasured", Folder, None),
                ],
            )
            .into(),
        );
        sizes
            .folder_sizes
            .insert(EntryName::Remote(b"a-measured".to_vec()), 1024);
        sizes.sort(SortColumn::Size);
        assert_eq!(labels(&sizes), ["b-unmeasured", "a-measured"]);
        sizes.sort(SortColumn::Size);
        assert_eq!(labels(&sizes), ["a-measured", "b-unmeasured"]);
    }

    #[test]
    fn sftp_account_display_resorts_resolved_owner_names_and_preserves_numeric_ids() {
        let directory = std::sync::Arc::new(Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            vec![
                remote("first", EntryKind::File, Some((1, 2, 0o100600))),
                remote("second", EntryKind::File, Some((1, 10, 0o100600))),
                remote("fallback", EntryKind::File, Some((1, 4, 0o100600))),
            ],
        ));
        let mut panel = Panel::default();
        panel.load(directory.clone());
        panel.sort(SortColumn::Owner);
        let selected = EntryName::Remote(b"first".to_vec());
        panel.toggle(selected, true);
        assert_eq!(labels(&panel), ["first", "fallback", "second"]);
        panel.accounts.begin(&directory, 1).unwrap();
        assert!(panel.accounts.finish(
            1,
            true,
            Some(DisplayNames {
                users: BTreeMap::from([(2, "Zulu".into()), (10, "alpha".into())]),
                groups: BTreeMap::new(),
            })
        ));
        panel.reorder();
        assert_eq!(labels(&panel), ["fallback", "second", "first"]);
        assert_eq!(panel.selected().unwrap().1.owner, Some(2));
        let names = panel.accounts.labels();
        assert_eq!(
            principal_label(Some(2), names.users.get(&2).map(String::as_str)),
            "Zulu"
        );
        assert_eq!(
            principal_label(Some(4), names.users.get(&4).map(String::as_str)),
            "4"
        );
        assert_eq!(principal_label(None, None), "—");
    }

    #[test]
    fn sftp_listing_reads_local_dates_without_changing_remote_wire_stamps() {
        let profile = crate::snippets::tests::Profile::new();
        let recent = profile.0.join("a-recent");
        let older = profile.0.join("z-older");
        let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (path, date) in [(&recent, base + Duration::from_secs(10)), (&older, base)] {
            File::create(path).unwrap().set_modified(date).unwrap();
        }
        fs::create_dir(profile.0.join(".config")).unwrap();
        let directory = files::list_local(&profile.0).unwrap();
        assert!(
            directory
                .entries
                .iter()
                .all(|entry| entry.modified.is_none())
        );
        let older = directory
            .entries
            .iter()
            .find(|entry| entry.display == "z-older")
            .unwrap();
        assert_eq!(directory.modified(older), Some(base));
        assert_eq!(
            date_label(directory.modified(older), &TimeZone::UTC),
            "2023-11-14 22:13:20"
        );
        let mut panel = Panel::default();
        panel.load(directory.into());
        panel.sort(SortColumn::Date);
        assert_eq!(labels(&panel), [".config", "z-older", "a-recent"]);
        panel.sort(SortColumn::Date);
        assert_eq!(labels(&panel), [".config", "a-recent", "z-older"]);
        assert_eq!(size_label(4096), "4.0 KiB");
    }

    #[test]
    fn sftp_listing_parent_navigation_preserves_raw_paths_and_skips_roots() {
        let local = PathBuf::from("parent").join("child");
        assert_eq!(
            Location::Local(local).parent(),
            Some(Location::Local("parent".into()))
        );
        assert_eq!(Location::Local(PathBuf::new()).parent(), None);
        assert_eq!(Location::Remote(b"/".to_vec()).parent(), None);
        assert_eq!(
            Location::Remote(b"/home/\xff".to_vec()).parent(),
            Some(Location::Remote(b"/home/\xff/..".to_vec()))
        );
    }
}
