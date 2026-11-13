use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, text, tooltip};
use iced::{Element, Length, Task};
use kortty_server_archive::manifest::{Member, MemberKind};

use super::worker::extract::{self, Plan, Request};
use super::{
    Directory, Entry, EntryKind, EntryName, Location, Message, Prompt, SftpWindow, worker,
};

const PAGE_SIZE: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Review,
    Apply,
}

pub(super) struct Selection {
    listing: Arc<Directory>,
    name: EntryName,
}

impl Selection {
    fn still_selected(&self, window: &SftpWindow) -> bool {
        window
            .remote
            .directory
            .as_ref()
            .is_some_and(|listing| Arc::ptr_eq(listing, &self.listing))
            && window.remote.selected.len() == 1
            && window.remote.selected.contains(&self.name)
    }
}

pub(super) struct Review {
    selection: Selection,
    plan: Arc<Plan>,
    page: usize,
}

impl Review {
    fn pages(&self) -> usize {
        self.plan
            .manifest()
            .members
            .len()
            .div_ceil(PAGE_SIZE)
            .max(1)
    }

    pub(super) fn set_page(&mut self, page: usize) {
        self.page = page.min(self.pages() - 1);
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let manifest = self.plan.manifest();
        let pages = self.pages();
        let page = self.page.min(pages - 1);
        let members = manifest
            .members
            .iter()
            .skip(page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .fold(column![].spacing(4), |rows, member| {
                rows.push(text(member_label(member)).wrapping(text::Wrapping::WordOrGlyph))
            });
        let mut form = column![
            text("Review server archive extraction").size(20),
            text(format!("Source · {}", self.plan.source())).wrapping(text::Wrapping::WordOrGlyph),
            text(format!("New folder · {}", self.plan.destination())).wrapping(text::Wrapping::WordOrGlyph),
            text(format!("{} members · {} expanded bytes", manifest.members.len(), manifest.expanded_bytes)),
            text("Extraction uses the native kortty archive helper on the server. The source archive stays in place."),
            scrollable(members).height(Length::Fixed(220.0)),
        ].padding(16).spacing(8);
        if pages > 1 {
            form = form.push(
                row![
                    button("Previous")
                        .on_press_maybe(page.checked_sub(1).map(Message::ExtractPage)),
                    text(format!("{} of {pages}", page + 1)),
                    button("Next").on_press_maybe(
                        (page + 1 < pages).then_some(Message::ExtractPage(page + 1))
                    ),
                ]
                .spacing(8),
            );
        }
        scrollable(
            form.push(
                row![
                    button("Cancel").on_press(Message::Dismiss),
                    button("Extract to new folder").on_press(Message::ApplyExtract),
                ]
                .spacing(8)
                .wrap(),
            ),
        )
        .into()
    }
}

fn member_label(member: &Member) -> String {
    let name = member.path.as_bytes().escape_ascii();
    match &member.kind {
        MemberKind::Directory => format!("Folder · {name}"),
        MemberKind::File => format!("File · {name} · {} bytes", member.size),
        MemberKind::Symlink { target } if target.is_empty() => {
            format!("Symbolic link · {name} → new folder root")
        }
        MemberKind::Symlink { target } => {
            format!(
                "Symbolic link · {name} → {}",
                target.as_bytes().escape_ascii()
            )
        }
        MemberKind::Hardlink { target } => {
            format!("Hard link · {name} → {}", target.as_bytes().escape_ascii())
        }
    }
}

impl SftpWindow {
    pub(super) fn extract_action(&self) -> Element<'_, Message> {
        tooltip(
            button("Extract here…").on_press_maybe(
                self.extract_candidate()
                    .is_some()
                    .then_some(Message::ExtractHere),
            ),
            text("Requires the native kortty archive helper on this server."),
            tooltip::Position::Top,
        )
        .into()
    }

    fn extract_candidate(&self) -> Option<(&worker::Worker, &Arc<Directory>, &Entry)> {
        if self.prompt.is_some() || self.comparison.is_some() || self.transport_busy() {
            return None;
        }
        let worker = self
            .worker
            .as_ref()
            .filter(|worker| extract::available(worker))?;
        let listing = self.remote.directory.as_ref()?;
        let (_, entry) = self.remote.selected()?;
        let (Location::Remote(directory), EntryName::Remote(name)) = (&listing.path, &entry.name)
        else {
            return None;
        };
        (directory.starts_with(b"/")
            && entry.kind == EntryKind::File
            && entry.ownership_reviewable()
            && extract::supported_name(name))
        .then_some((worker, listing, entry))
    }

    pub(super) fn begin_extract(&mut self) -> Task<Message> {
        let Some((worker, listing, entry)) = self.extract_candidate() else {
            return Task::none();
        };
        let Location::Remote(directory) = &listing.path else {
            return Task::none();
        };
        let selection = Selection {
            listing: Arc::clone(listing),
            name: entry.name.clone(),
        };
        let request = match Request::new(
            directory.clone(),
            entry.clone(),
            self.target.clone(),
            worker,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.status = Some(error.to_string());
                return Task::none();
            }
        };
        let task = self.dispatch(
            worker::Operation::ReviewServerExtract(
                Box::new(request),
                worker::sync::Cancel::default(),
            ),
            "Reviewing server archive extraction",
            None,
        );
        if let Some(busy) = &mut self.busy {
            busy.extract_review = Some(selection);
        }
        task
    }

    pub(super) fn finish_extract_review(&mut self, selection: Selection, plan: Arc<Plan>) {
        if !selection.still_selected(self) {
            self.status = Some("The archive selection changed. Select the archive again.".into());
            return;
        }
        if !self
            .worker
            .as_ref()
            .is_some_and(|worker| plan.applies_to(worker, &self.target))
        {
            self.status =
                Some("The reviewed SSH connection changed. Review extraction again.".into());
            return;
        }
        self.status = None;
        let review = Prompt::ServerExtractReview(Review {
            selection,
            plan,
            page: 0,
        });
        if let Some(Prompt::Close(previous)) = &mut self.prompt {
            *previous = Some(Box::new(review));
        } else {
            self.prompt = Some(review);
        }
    }

    pub(super) fn apply_extract(&mut self) -> Task<Message> {
        if self.transport_busy() {
            return Task::none();
        }
        let Some(Prompt::ServerExtractReview(review)) = &self.prompt else {
            return Task::none();
        };
        if !review.selection.still_selected(self) {
            self.prompt = None;
            self.status = Some("The archive selection changed. Select the archive again.".into());
            return Task::none();
        }
        if !self
            .worker
            .as_ref()
            .is_some_and(|worker| review.plan.applies_to(worker, &self.target))
        {
            self.prompt = None;
            self.status =
                Some("The reviewed SSH connection changed. Review extraction again.".into());
            return Task::none();
        }
        let plan = Arc::clone(&review.plan);
        self.prompt = None;
        self.dispatch(
            worker::Operation::ApplyServerExtract(plan, worker::sync::Cancel::default()),
            "Extracting server archive",
            None,
        )
    }

    pub(super) fn clear_extract_prompt(&mut self) {
        if matches!(self.prompt, Some(Prompt::ServerExtractReview(_))) {
            self.prompt = None;
        } else if let Some(Prompt::Close(previous)) = &mut self.prompt
            && matches!(previous.as_deref(), Some(Prompt::ServerExtractReview(_)))
        {
            *previous = None;
        }
    }
}
