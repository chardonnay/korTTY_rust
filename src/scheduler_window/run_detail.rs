use std::path::{Path, PathBuf};

use iced::keyboard::{Key, key::Named};
use iced::widget::{button, column, container, row, text, text_editor};
use iced::{Alignment, Element, Font, Length, Point};
use kortty_jobs::{RunId, RunRecord};

use crate::workspace::native_popup;

use super::{JournalDetails, Message, SchedulerView, SchedulerWindow};

pub(super) struct Sources<'a> {
    plain: [&'a str; 3],
    blockers: Vec<String>,
    linked: Vec<String>,
    linked_root: Option<&'a Path>,
}

impl<'a> Sources<'a> {
    pub(super) fn for_run(
        scheduler: &'a SchedulerWindow,
        record: &RunRecord,
        linked: Option<(&'a Path, &JournalDetails)>,
    ) -> Option<Self> {
        let [stdout, stderr] = scheduler
            .store
            .journal_output(record.id)
            .map_or(["", ""], |output| [&output.stdout, &output.stderr]);
        let plain = [
            stdout,
            stderr,
            scheduler
                .store
                .journal_detail(record.id)
                .unwrap_or_default(),
        ];
        let blockers: Vec<_> = record
            .blockers
            .iter()
            .map(|blocker| SchedulerView::blocker(scheduler, blocker))
            .collect();
        let (linked_root, linked) = match linked {
            Some((root, JournalDetails::Ready(report)))
                if scheduler.store.session_journals(record.id).is_some() =>
            {
                let labels = report
                    .rows
                    .iter()
                    .map(|row| super::view::journal_details::row_label(row, scheduler))
                    .collect::<Vec<_>>();
                ((!labels.is_empty()).then_some(root), labels)
            }
            _ => (None, Vec::new()),
        };
        (plain.iter().any(|source| !source.is_empty())
            || !blockers.is_empty()
            || !linked.is_empty())
        .then_some(Self {
            plain,
            blockers,
            linked,
            linked_root,
        })
    }
}

pub(super) struct Editor {
    pub(super) run: RunId,
    pub(super) content: text_editor::Content,
    pub(super) menu: Option<Point>,
    plain: [String; 3],
    blockers: Vec<String>,
    linked: Vec<String>,
    linked_root: Option<PathBuf>,
}

impl Editor {
    pub(super) fn new(run: RunId, sources: Sources<'_>) -> Self {
        use std::fmt::Write as _;

        let mut content = String::new();
        let [stdout, stderr, detail] = sources.plain;
        for (label, value) in [("stdout", stdout), ("stderr", stderr)] {
            if !value.is_empty() {
                begin_section(&mut content, label);
                let _ = write!(content, "\n{value}");
            }
        }
        if !detail.is_empty() || !sources.blockers.is_empty() {
            begin_section(&mut content, "Detail");
            if !detail.is_empty() {
                let _ = write!(content, "\n{detail}");
            }
            for blocker in &sources.blockers {
                let _ = write!(content, "\n{blocker}");
            }
        }
        if !sources.linked.is_empty() {
            begin_section(&mut content, "Session Journals");
            for label in &sources.linked {
                let _ = write!(content, "\n• {label}");
            }
        }
        Self {
            run,
            content: text_editor::Content::with_text(&content),
            menu: None,
            plain: sources.plain.map(str::to_owned),
            blockers: sources.blockers,
            linked: sources.linked,
            linked_root: sources.linked_root.map(Path::to_owned),
        }
    }

    pub(super) fn matches(&self, run: RunId, sources: &Sources<'_>) -> bool {
        self.run == run
            && self.plain.each_ref().map(String::as_str) == sources.plain
            && self.blockers == sources.blockers
            && self.linked == sources.linked
            && self.linked_root.as_deref() == sources.linked_root
    }

    pub(super) fn visible(&self, run: RunId, permitted_root: Option<&Path>) -> bool {
        self.run == run
            && self
                .linked_root
                .as_deref()
                .is_none_or(|root| Some(root) == permitted_root)
    }

    pub(super) fn selection(&self) -> Option<String> {
        self.content.selection().filter(|value| !value.is_empty())
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let run = self.run;
        let can_copy = self.content.cursor().selection.is_some();
        let editor = text_editor(&self.content)
            .font(Font::MONOSPACE)
            .size(13)
            .min_height(72)
            .max_height(175)
            .wrapping(text::Wrapping::WordOrGlyph)
            .on_action(move |action| Message::SelectRunDetail(run, action))
            .key_binding(move |press| {
                let binding = text_editor::Binding::from_key_press(press)?;
                Some(match binding {
                    text_editor::Binding::Unfocus
                    | text_editor::Binding::Move(_)
                    | text_editor::Binding::Select(_)
                    | text_editor::Binding::SelectWord
                    | text_editor::Binding::SelectLine
                    | text_editor::Binding::SelectAll => binding,
                    text_editor::Binding::Copy => {
                        text_editor::Binding::Custom(Message::CopyRunDetail(run))
                    }
                    _ => text_editor::Binding::Sequence(Vec::new()),
                })
            })
            .into();
        let copy = can_copy.then_some(Message::CopyRunDetail(run));
        let popup = self.menu.map(|point| {
            let contents = container(
                button("Copy selected")
                    .style(button::text)
                    .on_press_maybe(copy.clone()),
            )
            .padding(4)
            .style(container::rounded_box);
            (point, contents.into())
        });
        let editor = native_popup::context_action_with_keyboard(
            editor,
            move |point| Message::OpenRunDetailMenu(run, point),
            popup,
            Message::DismissRunDetailMenu(run),
            move |key, _| match key {
                Key::Named(Named::Escape) => Some(Message::DismissRunDetailMenu(run)),
                Key::Named(Named::Enter) if can_copy => Some(Message::CopyRunDetail(run)),
                _ => None,
            },
        );
        column![
            row![
                text("Run details").size(15).width(Length::Fill),
                button(text("Copy selected").size(12))
                    .style(button::secondary)
                    .on_press_maybe(copy),
            ]
            .align_y(Alignment::Center),
            editor,
        ]
        .spacing(3)
        .into()
    }
}

fn begin_section(content: &mut String, heading: &str) {
    if !content.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(heading);
}
