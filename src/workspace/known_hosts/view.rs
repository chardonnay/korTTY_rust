use std::path::Path;

use iced::widget::{button, column, container, row, rule, scrollable, space, text, text_input};
use iced::{Alignment, Color, Element, Font, Length, Theme, window};
use kortty_ssh::{ImportReport, TrustedHost};

use crate::sftp_window::{Directory, EntryKind, EntryName, Location};

use super::{Desktop, KnownHosts, Message, Mode, PAGE_SIZE, Workspace, wrap};

impl Workspace {
    pub(in crate::workspace) fn known_hosts_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let tool = self.known_hosts.get(&id)?;
        Some(
            tool.view(
                self.host_pins_locked(),
                self.clipboard.policy.problem.is_none(),
            )
            .map(move |message| wrap(id, message)),
        )
    }
}

impl KnownHosts {
    fn view(&self, locked: bool, can_import: bool) -> Element<'_, Message> {
        let body = match &self.mode {
            Mode::Table => self.table(locked, can_import),
            Mode::Remove(pin) => self.remove(pin, locked),
            Mode::Source { path, browse } => self.source(path, browse.as_deref()),
            Mode::Review(review) => self.report(review.report(), Some(can_import)),
            Mode::Summary(report) => self.report(report, None),
        };
        let mut content = column![body].spacing(8);
        if let Some(error) = &self.load_error {
            content =
                content.push(text(format!("Could not load trusted host keys ({error}).")).size(13));
        }
        if let Some(status) = &self.status {
            content = content.push(text(status).size(13));
        }
        if locked {
            content = content.push(
                text("Managed policy locks removal and replacement of trusted SSH keys.").size(13),
            );
        }
        content
            .push(row![
                space::horizontal(),
                button("Close").on_press_maybe((!self.writing()).then_some(Message::Close)),
            ])
            .padding(12)
            .height(Length::Fill)
            .into()
    }

    fn table(&self, locked: bool, can_import: bool) -> Element<'_, Message> {
        let needle = self.query.trim().to_ascii_lowercase();
        let mut lines = column![].spacing(1);
        let rows = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, row)| row.matches(&needle));
        let mut count = 0;
        for (index, item) in rows.skip(self.page * PAGE_SIZE).take(PAGE_SIZE) {
            let columns = table_row(
                text(item.pin.endpoint.host()).wrapping(text::Wrapping::None),
                text(item.pin.endpoint.port()),
                text(&item.algorithm),
                text(&item.fingerprint)
                    .font(Font::MONOSPACE)
                    .size(13)
                    .wrapping(text::Wrapping::None),
                text(&item.date),
            );
            let selected = self.selected == Some(index);
            lines = lines.push(
                button(columns)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(move |theme, status| row_style(theme, status, selected))
                    .on_press_maybe(self.ready().then(|| Message::Select(item.pin.clone()))),
            );
            count += 1;
        }
        if count == 0 && self.load_error.is_none() {
            let label = if self.busy() {
                "Loading trusted SSH host keys…"
            } else if self.entries.is_empty() {
                "There are no trusted SSH host keys."
            } else {
                "No trusted host keys match this search."
            };
            lines = lines.push(container(text(label).size(14)).padding(12));
        }
        let header = container(table_row(
            text("Host"),
            text("Port"),
            text("Algorithm"),
            text("SHA-256 fingerprint"),
            text("Trusted since"),
        ))
        .padding([7, 8])
        .width(Length::Fill)
        .style(|_| container::Style::default().background(Color::from_rgb8(48, 48, 48)));
        let grid = column![
            header,
            scrollable(lines).width(Length::Fill).height(Length::Fill)
        ]
        .height(Length::Fill);
        let controls = row![
            button("Import from known_hosts…")
                .on_press_maybe((self.ready() && can_import).then_some(Message::Import)),
            button("Remove…").on_press_maybe(
                (self.ready() && !locked && self.selected.is_some()).then_some(Message::Remove)
            ),
            button("Reload").on_press_maybe((!self.busy()).then_some(Message::Reload)),
            space::horizontal(),
            self.pagination(),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![
            text_input("Search host, port, algorithm or fingerprint", &self.query)
                .on_input(Message::Search),
            grid,
            controls,
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn remove(&self, pin: &TrustedHost, locked: bool) -> Element<'_, Message> {
        column![
            text(format!("Remove the trusted SSH key for {}?", pin.endpoint)).size(20),
            text("The next connection will require first-use verification again."),
            text(pin.key.algorithm()),
            text(pin.key.fingerprint()).font(Font::MONOSPACE),
            row![
                button("Cancel").on_press_maybe((!self.busy()).then_some(Message::Back)),
                button("Remove trusted key")
                    .style(button::danger)
                    .on_press_maybe((self.ready() && !locked).then_some(Message::ConfirmRemoval)),
            ]
            .spacing(8),
            space::vertical(),
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }

    fn source<'a>(&'a self, path: &'a str, browse: Option<&'a Directory>) -> Element<'a, Message> {
        let absolute = Path::new(path).is_absolute();
        let parent = Path::new(path).parent().filter(|path| path.is_absolute());
        let open_directory = parent.map(|path| Message::Browse(path.to_path_buf()));
        let fields = row![
            text_input("Absolute path to an OpenSSH known_hosts file", path)
                .on_input(Message::SourcePath)
                .on_submit_maybe((absolute && self.ready()).then_some(Message::Preview)),
            button("Browse…").on_press_maybe(open_directory.filter(|_| !self.busy())),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut body = column![text("Import from known_hosts").size(20), fields].spacing(8);
        if let Some(directory) = browse
            && let Location::Local(base) = &directory.path
        {
            let mut choices = column![
                text(directory.path.display()).size(13),
                button("Parent folder").on_press_maybe(
                    base.parent()
                        .filter(|_| !self.busy())
                        .map(|path| Message::Browse(path.to_path_buf()))
                ),
                rule::horizontal(1),
            ]
            .spacing(4);
            for entry in directory
                .entries
                .iter()
                .filter(|entry| matches!(entry.kind, EntryKind::Directory | EntryKind::File))
                .take(512)
            {
                let EntryName::Local(name) = &entry.name else {
                    continue;
                };
                let file = base.join(name);
                let action = if entry.kind == EntryKind::Directory {
                    file.to_str().map(|_| Message::Browse(file.clone()))
                } else {
                    file.to_str()
                        .map(|file| Message::SourcePath(file.to_owned()))
                };
                let label = if entry.kind == EntryKind::Directory {
                    format!("{}/", entry.display)
                } else {
                    entry.display.clone()
                };
                choices = choices.push(
                    button(text(label))
                        .width(Length::Fill)
                        .style(button::text)
                        .on_press_maybe(action.filter(|_| !self.busy())),
                );
            }
            body = body.push(scrollable(choices).height(Length::Fill));
        } else {
            body = body.push(space::vertical());
        }
        body.push(
            row![
                button("Cancel").on_press_maybe((!self.busy()).then_some(Message::Back)),
                button("Review file")
                    .on_press_maybe((absolute && self.ready()).then_some(Message::Preview)),
            ]
            .spacing(8),
        )
        .height(Length::Fill)
        .into()
    }

    fn report<'a>(
        &'a self,
        report: &'a ImportReport,
        preview: Option<bool>,
    ) -> Element<'a, Message> {
        let title = if preview.is_some() {
            "Review the OpenSSH import"
        } else {
            "OpenSSH import complete"
        };
        let summary = format!(
            "{} {} · {} already trusted · {} conflicts · {} alternative keys",
            report.added.len(),
            if preview.is_some() {
                "new keys"
            } else {
                "added"
            },
            report.already_trusted,
            report.conflicts.len(),
            report.additional_keys
        );
        let skipped = &report.skipped;
        let detail = format!(
            "Skipped · {} hashed · {} CA · {} host patterns · {} unsupported · {} malformed · {} revoked markers · {} revoked candidates",
            skipped.hashed,
            skipped.authorities,
            skipped.patterns,
            skipped.unsupported,
            skipped.malformed,
            skipped.revoked,
            skipped.revoked_candidates,
        );
        let mut header =
            column![text(title).size(20), text(summary), text(detail).size(13)].spacing(6);
        if !skipped.malformed_lines.is_empty() {
            let lines = skipped
                .malformed_lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let remaining = skipped
                .malformed
                .saturating_sub(skipped.malformed_lines.len());
            let suffix = if remaining == 0 {
                String::new()
            } else {
                format!(" and {remaining} more")
            };
            header = header.push(text(format!("Malformed lines {lines}{suffix}")).size(12));
        }
        if report.needs_review != 0 {
            header = header.push(text(format!("{} endpoints became available after review. Review the file again to import them.", report.needs_review)).size(13));
        }
        if preview.is_some() && !report.added.is_empty() {
            header = header.push(
                text("Confirm only keys whose fingerprints you verified through a trusted source.")
                    .size(13),
            );
        }
        let changes = report.added.iter().map(|host| {
            format!(
                "{}  {}  {}",
                host.endpoint,
                host.key.algorithm(),
                host.key.fingerprint(),
            )
        });
        let conflicts = report.conflicts.iter().map(|conflict| {
            format!(
                "Conflict at {} · source line {}\n    Trusted {}\n    In file {}",
                conflict.endpoint,
                conflict.line,
                conflict.trusted.fingerprint(),
                conflict.offered.fingerprint(),
            )
        });
        let revoked = report.trusted_but_revoked.iter().map(|host| {
            format!(
                "Still trusted and marked revoked in the file · {}  {}",
                host.endpoint,
                host.key.fingerprint(),
            )
        });
        let details = column(
            changes
                .chain(conflicts)
                .chain(revoked)
                .skip(self.page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .map(|line| text(line).font(Font::MONOSPACE).size(13).into()),
        )
        .spacing(8);
        let mut controls = row![
            button(if preview.is_some() { "Cancel" } else { "Back" })
                .on_press_maybe((!self.busy()).then_some(Message::Back))
        ]
        .spacing(8);
        if let Some(can_import) = preview {
            controls = controls.push(
                button("Import verified keys").on_press_maybe(
                    (can_import && self.ready() && !report.added.is_empty())
                        .then_some(Message::ApplyImport),
                ),
            );
        }
        controls = controls.push(space::horizontal()).push(self.pagination());
        column![
            header,
            scrollable(details).height(Length::Fill).width(Length::Fill),
            controls
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn pagination(&self) -> Element<'_, Message> {
        let pages = self.visible_count().div_ceil(PAGE_SIZE);
        if pages < 2 {
            return space::horizontal().width(0).into();
        }
        row![
            button("‹")
                .on_press_maybe((!self.busy() && self.page > 0).then_some(Message::Page(false))),
            text(format!("{} / {pages}", self.page + 1)).size(13),
            button("›").on_press_maybe(
                (!self.busy() && self.page + 1 < pages).then_some(Message::Page(true))
            ),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .into()
    }
}

fn table_row<'a>(
    host: text::Text<'a>,
    port: text::Text<'a>,
    algorithm: text::Text<'a>,
    fingerprint: text::Text<'a>,
    date: text::Text<'a>,
) -> Element<'a, Message> {
    row![
        host.width(180),
        port.width(60),
        algorithm.width(152),
        fingerprint.width(Length::Fill),
        date.width(133),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

fn row_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let color = if selected {
        Color::from_rgb8(46, 63, 86)
    } else if status == button::Status::Hovered {
        Color::from_rgb8(53, 55, 58)
    } else {
        Color::from_rgb8(38, 38, 38)
    };
    button::text(theme, status).with_background(color)
}
