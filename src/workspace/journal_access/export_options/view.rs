use iced::widget::{
    Column, button, checkbox, column, container, pick_list, radio, row, scrollable, space, text,
    text_input, tooltip,
};
use iced::{Element, Length, Theme};

use super::*;

impl Draft {
    pub(in crate::workspace::journal_access) fn view(&self) -> Element<'_, Event> {
        let encryption = pick_list(
            [
                ProtectionMode::None,
                ProtectionMode::Password,
                ProtectionMode::OpenPgp,
            ],
            Some(self.protection),
            Event::Protection,
        );
        let include_chats = tooltip(
            checkbox(self.include_chats)
                .label("Include AI chats")
                .on_toggle(Event::IncludeChats),
            "The agent card stays in the export. This option controls its continued AI chat and steps.",
            tooltip::Position::Bottom,
        );
        let toggles = row![
            self.format
                .offers_screenshots()
                .then(|| checkbox(self.include_screenshots)
                    .label("Include screenshots")
                    .on_toggle(Event::IncludeScreenshots)),
            include_chats,
            row![text("Encryption"), encryption]
                .spacing(7)
                .align_y(iced::Alignment::Center),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center)
        .wrap();
        let filter = self.filter();
        let can_confirm = filter.as_ref().is_ok_and(|filter| self.can_confirm(filter));
        let topic_ai = checkbox(self.topic_ai)
            .label("Let the AI choose the entries")
            .on_toggle_maybe(self.topic_ai_available.then_some(Event::TopicAi));
        let topic_ai: Element<'_, Event> = if self.topic_ai_available {
            topic_ai.into()
        } else {
            tooltip(
                topic_ai,
                "No AI profile is available, or your organisation's policy does not allow it.",
                tooltip::Position::Bottom,
            )
            .into()
        };
        let mut controls = column![
            toggles,
            self.protection_fields(),
            section("Time windows", self.time_windows()),
            section(
                "Topic",
                column![
                    text_input("e.g. installing apache", &self.topic)
                        .on_input(Event::Topic)
                        .on_submit_maybe(can_confirm.then_some(Event::Confirm)),
                    row![
                        checkbox(self.topic_regex)
                            .label("Regular expression")
                            .on_toggle_maybe((!self.topic_ai).then_some(Event::TopicRegex)),
                        topic_ai,
                    ]
                    .spacing(12)
                    .wrap(),
                ]
                .spacing(7),
            ),
            section("Markers", self.marker_fields()),
        ]
        .spacing(10);
        if let Some((status, error)) = self.preview_status(filter.as_ref().ok()) {
            let message = text(status).size(13);
            controls = controls.push(if error {
                message.style(text::danger)
            } else {
                message
            });
        }
        let header = if self.journals == 1 {
            self.format.to_string()
        } else {
            format!(
                "{} — {} journals, each stored separately in the archive",
                self.format, self.journals
            )
        };
        container(
            column![
                text(header).size(17),
                scrollable(controls.padding(2)).height(Length::Fill),
                row![
                    space::horizontal(),
                    button("Cancel").on_press(Event::Cancel),
                    button("OK").on_press_maybe(can_confirm.then_some(Event::Confirm)),
                ]
                .spacing(8),
            ]
            .spacing(12),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(14)
        .into()
    }

    fn protection_fields(&self) -> Option<Element<'_, Event>> {
        match self.protection {
            ProtectionMode::None => None,
            ProtectionMode::Password => {
                let problem = if self.password_too_long || self.repeat_too_long {
                    Some(format!("Passwords must use at most {MAX_PASSWORD_BYTES} UTF-8 bytes."))
                } else if !self.repeat.as_str().is_empty() && !self.passwords_match() {
                    Some("The passwords do not match.".into())
                } else {
                    None
                };
                Some(
                    column![
                        row![
                            text_input("Password", self.password.as_str())
                                .secure(true)
                                .on_input(|value| Event::Password(Password::new(value))),
                            text_input("Repeat password", self.repeat.as_str())
                                .secure(true)
                                .on_input(|value| Event::Repeat(Password::new(value))),
                        ]
                        .spacing(8),
                        problem.map(|problem| text(problem).style(text::danger).size(13)),
                        text("The journal and its assets are stored in a password-protected ZIP archive.").size(13),
                    ]
                    .spacing(6)
                    .into(),
                )
            }
            ProtectionMode::OpenPgp if self.recipients.is_empty() => Some(
                text("No verified keys. Add one under Security → OpenPGP recipients.")
                    .size(13)
                    .into(),
            ),
            ProtectionMode::OpenPgp => Some(
                column![
                    pick_list(&self.recipients[..], self.recipient.as_ref(), Event::Recipient)
                        .placeholder("Select a GPG key"),
                    text(if self.format == Format::Pdf && self.journals == 1 {
                        "The PDF is encrypted as an OpenPGP document."
                    } else {
                        "The journal and its assets are encrypted together as an OpenPGP ZIP archive."
                    })
                        .size(13),
                ]
                .spacing(6)
                .into(),
            ),
        }
    }

    fn time_windows(&self) -> Column<'_, Event> {
        let mut rows = column![].spacing(7);
        let mut midnight = false;
        for window in &self.windows {
            rows = rows.push(window.view());
            if let Ok(Some(window)) = window.parse() {
                midnight |= matches!((window.from_time, window.to_time), (Some(from), Some(to)) if from > to);
            }
        }
        let tolerance = self.tolerance();
        rows.push(
            row![
                button("Add window")
                    .on_press_maybe((self.windows.len() < MAX_WINDOWS).then_some(Event::AddWindow)),
                button("Whole journal")
                    .on_press_maybe((!self.windows.is_empty()).then_some(Event::ClearWindows)),
                self.can_pick_range
                    .then(|| button("Pick from journal…").on_press(Event::PickRange)),
            ]
            .spacing(8),
        )
        .push(
            row![
                text("Tolerance ±"),
                text_input("0–120", &self.tolerance)
                    .on_input(Event::Tolerance)
                    .width(58)
                    .style(move |theme, status| input_style(theme, status, tolerance.is_none())),
                button("−")
                    .on_press_maybe((tolerance != Some(0)).then_some(Event::StepTolerance(false))),
                button("+").on_press_maybe(
                    (tolerance != Some(MAX_TOLERANCE_MINUTES))
                        .then_some(Event::StepTolerance(true)),
                ),
                text("min"),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .push(midnight.then(|| {
            text("A window whose start is later than its end runs across midnight.").size(13)
        }))
    }

    fn marker_fields(&self) -> Column<'_, Event> {
        let mut modes = row![].spacing(12);
        for (mode, label) in [
            (MarkerMode::All, "All entries"),
            (MarkerMode::Marked, "Only marked entries"),
            (MarkerMode::Selected, "Only these markers"),
        ] {
            let selector: Element<'_, Event> = if mode != MarkerMode::All && self.markers.is_empty()
            {
                text(format!("○ {label}")).style(text::secondary).into()
            } else {
                radio(label, mode, Some(self.marker_mode), Event::MarkerMode).into()
            };
            modes = modes.push(selector);
        }
        let mut markers = row![].spacing(10);
        for (id, name) in &self.markers {
            markers = markers.push(
                checkbox(self.marker_ids.contains(id))
                    .label(name)
                    .on_toggle_maybe((self.marker_mode == MarkerMode::Selected).then(|| {
                        let id = id.clone();
                        move |enabled| Event::Marker(id.clone(), enabled)
                    })),
            );
        }
        column![
            modes.wrap(),
            (!self.markers.is_empty()).then(|| markers.wrap()),
            self.marker_list_limited.then(|| {
                text(format!("Showing the first {MAX_PREVIEW_MARKERS} markers.")).size(13)
            }),
        ]
        .spacing(8)
    }

    pub(super) fn preview_status(&self, filter: Option<&Filter>) -> Option<(String, bool)> {
        let Some(filter) = filter else {
            return self.filter().err().map(|problem| (problem.into(), true));
        };
        if self.uses_ai_topic(filter) {
            return Some(("Determined by the AI during the export".into(), false));
        }
        match &self.preview {
            Some(Counted::InvalidFilter) => {
                return Some((
                    if needs_regex_check(filter) {
                        "This is not a valid regular expression."
                    } else {
                        "Check the selected export filters."
                    }
                    .into(),
                    true,
                ));
            }
            Some(Counted::Entries {
                kept,
                total,
                journals,
            }) => {
                let status = if *journals == 1 {
                    format!("{kept} of {total} entries match")
                } else {
                    format!("{kept} of {total} entries in {journals} journals match")
                };
                return Some((
                    status,
                    *kept == 0 && *journals == self.journals && filter.is_active(),
                ));
            }
            Some(Counted::Failed(problem)) => {
                return Some((
                    format!(
                        "Could not count entries: {}",
                        super::super::label(problem, 160)
                    ),
                    true,
                ));
            }
            Some(Counted::WorkerFailed) => {
                return Some(("Could not validate the export preview.".into(), true));
            }
            _ => {}
        }
        let status = match &self.preview_sources {
            _ if self.preview_job.is_some() => "Counting…".into(),
            PreviewSources::Loading | PreviewSources::Loaded(_) => "Counting…".into(),
            PreviewSources::Unavailable => {
                "Too many journals for a live preview. The filter is applied during export.".into()
            }
            PreviewSources::Failed(problem) => {
                return Some((
                    format!(
                        "Could not load the preview: {}",
                        super::super::label(problem, 160)
                    ),
                    true,
                ));
            }
        };
        Some((status, false))
    }
}

impl WindowDraft {
    fn view(&self) -> Element<'_, Event> {
        let id = self.id;
        let from_date = parse_date(&self.from_date);
        let to_date = parse_date(&self.to_date);
        let reversed =
            matches!((&from_date, &to_date), (Ok(Some(from)), Ok(Some(to))) if from > to);
        let field = |bound, prompt: &'static str, value: &str, invalid, width: u32| {
            text_input(prompt, value)
                .on_input(move |value| Event::Bound(id, bound, value))
                .width(width)
                .style(move |theme, status| input_style(theme, status, invalid))
        };
        row![
            text("From").size(13),
            field(
                Bound::FromDate,
                "YYYY-MM-DD",
                &self.from_date,
                from_date.is_err() || reversed,
                116
            ),
            field(
                Bound::FromTime,
                "HH:MM",
                &self.from_time,
                parse_time_of_day(&self.from_time).is_err(),
                76
            ),
            text("To").size(13),
            field(
                Bound::ToDate,
                "YYYY-MM-DD",
                &self.to_date,
                to_date.is_err() || reversed,
                116
            ),
            field(
                Bound::ToTime,
                "HH:MM",
                &self.to_time,
                parse_time_of_day(&self.to_time).is_err(),
                76
            ),
            tooltip(
                button("✕").on_press(Event::RemoveWindow(id)),
                "Remove this window",
                tooltip::Position::Bottom
            ),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .wrap()
        .into()
    }
}

fn section<'a>(title: &'static str, content: impl Into<Element<'a, Event>>) -> Element<'a, Event> {
    container(column![text(title).size(15), content.into()].spacing(8))
        .padding(9)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}

fn input_style(theme: &Theme, status: text_input::Status, invalid: bool) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    if invalid {
        style.border.color = theme.extended_palette().danger.strong.color;
    }
    style
}
