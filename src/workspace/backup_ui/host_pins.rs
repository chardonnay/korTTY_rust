//! Separate approval of the public host keys found in a Java backup.

use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;

use iced::widget::{button, checkbox, column, row, scrollable, space, text};
use iced::{Element, Font, Length, Task};
use kortty_ssh::ImportReview;

use super::{Desktop, Dialog, HostPinReply, Message, PAGE_SIZE, Phase, wrap};
use crate::workspace::known_hosts;

pub(super) struct Review {
    state: State,
    page: usize,
    selected: BTreeSet<usize>,
}

enum State {
    Preparing,
    Ready(Arc<ImportReview>),
    Applying(Arc<ImportReview>),
}

impl Review {
    pub(super) fn busy(&self) -> bool {
        !matches!(self.state, State::Ready(_))
    }
}

impl Dialog {
    pub(super) fn review_host_pins(&mut self, revision: u64) -> Task<Desktop> {
        if revision != self.revision
            || self.busy()
            || self.discard.is_some()
            || self.host_pins.is_some()
        {
            return Task::none();
        }
        let Phase::Review(preview) = &self.phase else {
            return Task::none();
        };
        let Some(source) = preview.java_host_pins() else {
            return Task::none();
        };
        self.host_pins = Some(Review {
            state: State::Preparing,
            page: 0,
            selected: BTreeSet::new(),
        });
        self.start_host_pin_work(
            move || {
                known_hosts::preview_import(source.as_ref().clone())
                    .map(Arc::new)
                    .map_err(error_text)
            },
            HostPinReply::Reviewed,
        )
    }

    pub(super) fn update_host_pins(&mut self, message: Message) -> Task<Desktop> {
        let Some(review) = self.host_pins.as_mut() else {
            return Task::none();
        };
        let State::Ready(approved) = &review.state else {
            return Task::none();
        };
        match message {
            Message::SelectHostPin(revision, index, selected)
                if revision == self.revision && index < approved.report().added.len() =>
            {
                if selected {
                    review.selected.insert(index);
                } else {
                    review.selected.remove(&index);
                }
            }
            Message::HostPinsPage(revision, forward) if revision == self.revision => {
                let max = details(approved).div_ceil(PAGE_SIZE).saturating_sub(1);
                review.page = if forward {
                    (review.page + 1).min(max)
                } else {
                    review.page.saturating_sub(1)
                };
            }
            Message::BackFromHostPins(revision) if revision == self.revision => {
                self.revision = self.revision.wrapping_add(1);
                self.host_pins = None;
                self.error = None;
            }
            Message::ApplyHostPins(revision)
                if revision == self.revision && !review.selected.is_empty() =>
            {
                let approved = approved.clone();
                let selected = review.selected.clone();
                review.state = State::Applying(approved.clone());
                return self.start_host_pin_work(
                    move || {
                        known_hosts::import_reviewed_selected(&approved, &selected)
                            .map(Arc::new)
                            .map_err(error_text)
                    },
                    HostPinReply::Imported,
                );
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn finish_host_pins(&mut self, revision: u64, reply: HostPinReply) -> bool {
        if revision != self.revision {
            return false;
        }
        let Some(review) = self.host_pins.as_mut() else {
            return false;
        };
        match (&review.state, reply) {
            (State::Preparing, HostPinReply::Reviewed(Ok(approved))) => {
                review.state = State::Ready(approved);
            }
            (State::Preparing, HostPinReply::Reviewed(Err(error))) => {
                self.host_pins = None;
                self.error = Some(format!("Could not review SSH host keys ({error})."));
            }
            (State::Applying(_), HostPinReply::Imported(Ok(report))) => {
                self.host_pins = None;
                self.imported_host_pins = Some(report);
                return true;
            }
            (State::Applying(approved), HostPinReply::Imported(Err(error))) => {
                review.state = State::Ready(approved.clone());
                self.error = Some(format!("Could not import SSH host keys ({error})."));
            }
            _ => {}
        }
        false
    }

    fn start_host_pin_work<T: Send + 'static>(
        &mut self,
        run: impl FnOnce() -> Result<T, String> + Send + 'static,
        reply: fn(Result<T, String>) -> HostPinReply,
    ) -> Task<Desktop> {
        self.revision = self.revision.wrapping_add(1);
        self.error = None;
        let revision = self.revision;
        let id = self.id;
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                if let Err(error) = std::thread::Builder::new()
                    .name("kortty-host-key-import".into())
                    .spawn(move || {
                        let _ = send.send_blocking(run());
                    })
                {
                    return Err(error.to_string());
                }
                receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The host key worker stopped.".into()))
            },
            move |result| wrap(id, Message::HostPinsFinished(revision, reply(result))),
        )
    }

    pub(super) fn host_pin_view<'a>(
        &'a self,
        review: &'a Review,
        can_import: bool,
    ) -> Element<'a, Message> {
        let State::Ready(approved) = &review.state else {
            let status = if matches!(review.state, State::Preparing) {
                "Reviewing SSH host keys…"
            } else {
                "Importing verified SSH host keys…"
            };
            return text(status).size(23).into();
        };
        let report = approved.report();
        let revision = self.revision;
        let additions = report.added.iter().enumerate().map(|(index, host)| {
            let toggle = checkbox(review.selected.contains(&index))
                .label(known_hosts::review_key_label(host))
                .font(Font::MONOSPACE)
                .text_size(13);
            toggle
                .on_toggle(move |checked| Message::SelectHostPin(revision, index, checked))
                .into()
        });
        let conflicts = report.conflicts.iter().map(|conflict| {
            text(known_hosts::review_conflict_label(conflict, false))
                .font(Font::MONOSPACE)
                .size(13)
                .into()
        });
        let lines = column(
            additions
                .chain(conflicts)
                .skip(review.page * PAGE_SIZE)
                .take(PAGE_SIZE),
        )
        .spacing(8);
        let pages = details(approved).div_ceil(PAGE_SIZE);
        column![
            text("Review SSH Host Keys").size(23),
            text(format!(
                "{} new keys · {} already trusted · {} conflicts",
                report.added.len(),
                report.already_trusted,
                report.conflicts.len()
            )),
            text(
                "Verify each fingerprint through a trusted source, then select the keys to import."
            )
            .size(13),
            text(format!("{} selected", review.selected.len())).size(13),
            self.error.as_ref().map(|error| text(error).size(13)),
            scrollable(lines).height(Length::Fill).width(Length::Fill),
            row![
                button("Back").on_press(Message::BackFromHostPins(revision)),
                (pages > 1).then(|| row![
                    button("‹").on_press_maybe(
                        (review.page != 0).then_some(Message::HostPinsPage(revision, false))
                    ),
                    text(format!("{} / {pages}", review.page + 1)),
                    button("›").on_press_maybe(
                        (review.page + 1 < pages).then_some(Message::HostPinsPage(revision, true))
                    ),
                ]
                .spacing(4)),
                space::horizontal(),
                button("Import Selected Keys")
                    .style(button::primary)
                    .on_press_maybe(
                        (can_import && !review.selected.is_empty())
                            .then_some(Message::ApplyHostPins(revision))
                    ),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }
}

fn details(review: &ImportReview) -> usize {
    let report = review.report();
    report.added.len() + report.conflicts.len()
}

fn error_text(error: io::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use kortty_ssh::{ImportReport, KnownHostsFile, TrustStore};

    use super::*;
    use crate::workspace::backup_ui::Kind;
    use iced::window;

    #[test]
    fn stale_messages_never_authorize_a_host_pin_import() {
        let mut dialog = Dialog::new(window::Id::unique(), window::Id::unique(), Kind::Import);
        let store = TrustStore::from_pins(vec![]).unwrap();
        let source = KnownHostsFile::parse(concat!(
            "example.test ssh-ed25519 ",
            "AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX\n",
        ))
        .unwrap();
        let review = Arc::new(store.preview_import(source).unwrap());
        dialog.revision = 4;
        dialog.host_pins = Some(Review {
            state: State::Preparing,
            page: 0,
            selected: BTreeSet::new(),
        });
        assert!(!dialog.finish_host_pins(3, HostPinReply::Reviewed(Ok(review.clone()))));
        assert!(dialog.busy());
        assert!(!dialog.finish_host_pins(
            4,
            HostPinReply::Imported(Ok(Arc::new(ImportReport::default())))
        ));
        assert!(dialog.busy());
        assert!(!dialog.finish_host_pins(4, HostPinReply::Reviewed(Ok(review.clone()))));
        assert!(!dialog.busy());
        let _ = dialog.update_host_pins(Message::SelectHostPin(3, 0, true));
        let _ = dialog.update_host_pins(Message::SelectHostPin(4, 1, true));
        let _ = dialog.update_host_pins(Message::ApplyHostPins(4));
        assert!(!dialog.busy());
        assert!(dialog.host_pins.as_ref().unwrap().selected.is_empty());
        let _ = dialog.update_host_pins(Message::SelectHostPin(4, 0, true));
        let _ = dialog.update_host_pins(Message::HostPinsPage(4, true));
        assert_eq!(
            dialog.host_pins.as_ref().unwrap().selected,
            BTreeSet::from([0])
        );
        let _ = dialog.update_host_pins(Message::ApplyHostPins(3));
        assert!(!dialog.busy());
        dialog.host_pins.as_mut().unwrap().state = State::Applying(review);
        assert!(!dialog.finish_host_pins(4, HostPinReply::Imported(Err("Policy changed".into()))));
        assert!(!dialog.busy());
        assert!(dialog.error.as_deref().unwrap().contains("Policy changed"));
        let _ = dialog.update_host_pins(Message::BackFromHostPins(4));
        assert!(dialog.host_pins.is_none());
        assert!(dialog.error.is_none());
    }
}
