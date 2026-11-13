//! Runs queued transfers on independently owned channels of the browser's SSH peer.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use iced::Task;

use super::{Location, Message, Prompt, RemoteEditorPolicy, SftpWindow, workbench, worker};

pub(crate) const DEFAULT_PARALLEL: u8 = 3;
pub(crate) const MAX_PARALLEL: u8 = (kortty_ssh::MAX_SFTP_LEASES - 1) as u8;
const ATTACHED_PARALLEL: u8 = 2;

pub(crate) fn managed_cap(managed: Option<u8>) -> u8 {
    managed.unwrap_or(MAX_PARALLEL).clamp(1, MAX_PARALLEL)
}

pub(super) fn browsing_message(message: &Message) -> bool {
    matches!(
        message,
        Message::Path(..)
            | Message::Filter(..)
            | Message::Hidden(..)
            | Message::Sort(..)
            | Message::Page(..)
            | Message::Select(..)
            | Message::ToggleSelection(..)
            | Message::Open(..)
            | Message::Go(..)
            | Message::Up(..)
            | Message::Home(..)
            | Message::Refresh(..)
            | Message::Breadcrumb(..)
            | Message::Transfer(..)
    )
}

#[derive(Default)]
pub(super) struct Transfers {
    limit: Option<u8>,
    lanes: BTreeMap<workbench::Id, Lane>,
    conflicts: VecDeque<(workbench::Id, u64, worker::Conflict)>,
    offers: VecDeque<(Option<workbench::Id>, worker::ResumeOffer)>,
    refresh: bool,
}

struct Lane {
    ticket: u64,
    worker: worker::Worker,
    progress: worker::Progress,
    cancelled: bool,
}

impl Transfers {
    pub(super) fn active(&self) -> bool {
        !self.lanes.is_empty() || !self.conflicts.is_empty()
    }

    pub(super) fn reset(&mut self, target: &crate::workspace::SshTarget) {
        let _ = self.stop(target);
    }

    pub(super) fn stop(
        &mut self,
        target: &crate::workspace::SshTarget,
    ) -> Vec<async_channel::Receiver<()>> {
        let lanes = std::mem::take(&mut self.lanes);
        let mut stopped = Vec::with_capacity(lanes.len());
        for (_, lane) in lanes {
            lane.worker.configure_resume(None, target);
            stopped.push(lane.worker.stop());
        }
        self.conflicts.clear();
        self.refresh = false;
        stopped
    }

    pub(super) fn retain_offers(&mut self, peer: Option<&kortty_ssh::HostKey>) {
        self.offers
            .retain(|(_, offer)| offer.matches_peer(peer) && offer.available());
    }

    pub(super) fn clear_offers(&mut self) {
        self.offers.clear();
    }

    pub(super) fn configure_resume_preferences(&self, preferences: super::ResumePreferences) {
        for lane in self.lanes.values() {
            lane.worker.set_resume_preferences(preferences);
        }
    }

    pub(super) fn retain_offer(&mut self, row: Option<workbench::Id>, offer: worker::ResumeOffer) {
        if offer.available() && self.offers.len() < usize::from(MAX_PARALLEL) {
            self.offers.push_back((row, offer));
        }
    }

    pub(super) fn partial_ids(&self) -> Vec<workbench::Id> {
        self.offers
            .iter()
            .filter(|(_, offer)| offer.available())
            .filter_map(|(id, _)| *id)
            .collect()
    }

    pub(super) fn retry_offers(
        &self,
    ) -> impl Iterator<Item = (workbench::Id, worker::ResumeOffer)> {
        self.offers.iter().filter_map(|(id, offer)| {
            let id = (*id)?;
            offer.available().then(|| (id, offer.clone()))
        })
    }

    pub(super) fn prune_offers(&mut self) {
        self.offers.retain(|(_, offer)| offer.available());
    }

    pub(super) fn promote_offer(
        &mut self,
        current: &mut Option<worker::ResumeOffer>,
        row: &mut Option<workbench::Id>,
    ) {
        if current.is_none() {
            *row = None;
            while let Some((id, offer)) = self.offers.pop_front() {
                if offer.available() {
                    *current = Some(offer);
                    *row = id;
                    break;
                }
            }
        }
    }

    pub(super) fn can_cancel(&self, id: workbench::Id) -> bool {
        self.lanes.get(&id).is_some_and(|lane| !lane.cancelled)
    }

    pub(super) fn has_lane(&self, id: workbench::Id) -> bool {
        self.lanes.contains_key(&id)
    }

    pub(super) fn progress(&self, id: workbench::Id) -> Option<String> {
        let lane = self.lanes.get(&id)?;
        Some(if lane.cancelled {
            "Stopping transfer…".into()
        } else {
            lane.progress.label()
        })
    }

    pub(super) fn cancel(&mut self, id: workbench::Id) {
        if let Some(lane) = self.lanes.get_mut(&id) {
            lane.cancelled = true;
            lane.progress.stop(&lane.worker);
        }
        self.conflicts.retain(|(row, _, _)| *row != id);
    }
}

impl SftpWindow {
    pub(crate) fn configure_parallel(&mut self, preferred: u8, managed: Option<u8>) {
        let limit = preferred.clamp(1, managed_cap(managed));
        // A lower cap limits new work. Existing writes and reviewed conflicts
        // drain under their original ownership instead of being cancelled.
        self.parallel.limit = Some(limit);
    }

    fn parallel_limit(&self) -> u8 {
        let limit = self.parallel.limit.unwrap_or(1);
        if self.attached_to.is_some() {
            limit.min(ATTACHED_PARALLEL)
        } else {
            limit
        }
    }

    pub(super) fn parallel_saturated(&self) -> bool {
        self.parallel.lanes.len() >= usize::from(self.parallel_limit())
    }

    pub(super) fn parallel_available(&self) -> bool {
        self.parallel_limit() > 1
            && self
                .worker
                .as_ref()
                .and_then(worker::Worker::transfer_opener)
                .is_some()
    }

    pub(super) fn schedule_parallel(&mut self) -> Task<Message> {
        self.update_partial_rows();
        if self.busy.is_some()
            || self.prompt.is_some()
            || self.comparison.is_some()
            || self.external.busy()
        {
            return Task::none();
        }
        if !self.parallel.conflicts.is_empty() && self.parallel_saturated() {
            return Task::none();
        }
        if let Some((row, ticket, conflict)) = self.parallel.conflicts.pop_front() {
            if let Some(choice) = self.workbench.conflict_choice(row, &conflict) {
                return self.apply_resolution(row, conflict, choice, false);
            }
            self.prompt = Some(Prompt::Conflict {
                row,
                ticket,
                conflict,
                apply_all: false,
            });
            return Task::none();
        }
        let limit = usize::from(self.parallel_limit());
        let mut tasks = Vec::new();
        while self.queue_available()
            && self.parallel_available()
            && self.parallel.lanes.len() < limit
        {
            let Some((id, pending)) = self.workbench.next_at(limit) else {
                break;
            };
            tasks.push(self.start_pending_transfer(id, pending));
        }
        if tasks.is_empty() && !self.parallel.active() && self.parallel.refresh {
            self.parallel.refresh = false;
            let local = self
                .local
                .directory
                .as_ref()
                .and_then(|dir| match &dir.path {
                    Location::Local(path) => Some(path.clone()),
                    Location::Remote(_) => None,
                });
            let remote = self
                .remote
                .directory
                .as_ref()
                .and_then(|dir| match &dir.path {
                    Location::Remote(path) => Some(path.clone()),
                    Location::Local(_) => None,
                });
            return self.dispatch(
                worker::Operation::Initial { local, remote },
                "Refreshing transfer destinations",
                None,
            );
        }
        if tasks.is_empty() && !self.parallel.active() && !self.parallel_available() {
            // After extra channels drain, cap one uses the browser worker. This
            // also supports servers which allow only their original subsystem.
            return self.start_next_transfer();
        }
        Task::batch(tasks)
    }

    pub(super) fn start_parallel(
        &mut self,
        id: workbench::Id,
        transfer: worker::Transfer,
        destination: worker::Destination,
    ) -> Task<Message> {
        let progress = self.workbench.progress(id, transfer.size);
        let operation = worker::Operation::queued_copy(transfer, destination, progress.clone());
        self.submit_parallel(id, operation, progress)
    }

    pub(super) fn submit_parallel(
        &mut self,
        id: workbench::Id,
        operation: worker::Operation,
        progress: worker::Progress,
    ) -> Task<Message> {
        let resume_offer = match &operation {
            worker::Operation::ResumeDownload { offer, .. } => Some(offer.clone()),
            _ => None,
        };
        let result = (|| {
            let opener = self
                .worker
                .as_ref()
                .and_then(worker::Worker::transfer_opener)
                .ok_or_else(|| {
                    std::io::Error::other("The authenticated transfer channel is unavailable")
                })?;
            let channel = worker::Worker::start(
                worker::Connection::Terminal(opener),
                RemoteEditorPolicy::Deny,
            )?;
            channel.bind_policy_route(&self.target);
            channel.set_resume_preferences(self.resume_preferences);
            let profile = self
                .resume_profile
                .as_deref()
                .filter(|_| !self.incognito && self.transfer_allowed);
            channel.configure_resume(profile, &self.target);
            let reply = channel.submit(operation)?;
            self.counter = self.counter.wrapping_add(1);
            let ticket = self.counter;
            self.parallel.lanes.insert(
                id,
                Lane {
                    ticket,
                    worker: channel,
                    progress,
                    cancelled: false,
                },
            );
            Ok::<_, std::io::Error>((ticket, reply))
        })();
        match result {
            Ok((ticket, reply)) => Task::future(async move {
                Message::TransferDone(
                    id,
                    ticket,
                    reply.recv().await.unwrap_or_else(|_| {
                        Err(worker::Failure::disconnected(
                            "The individual SFTP transfer channel closed",
                        ))
                    }),
                )
            }),
            Err(error) => {
                self.workbench.finish_parallel(
                    id,
                    workbench::Outcome::Failed {
                        message: error.to_string(),
                        partial: resume_offer.is_some_and(|offer| offer.available()),
                    },
                );
                Task::none()
            }
        }
    }

    pub(super) fn finish_parallel(
        &mut self,
        id: workbench::Id,
        ticket: u64,
        reply: Result<Arc<worker::Reply>, worker::Failure>,
    ) -> Task<Message> {
        if self
            .parallel
            .lanes
            .get(&id)
            .is_none_or(|lane| lane.ticket != ticket)
        {
            return Task::none();
        }
        let lane = self
            .parallel
            .lanes
            .remove(&id)
            .expect("matching transfer lane");
        self.promote_finished_offer();
        self.parallel.prune_offers();
        match reply {
            Ok(reply) => match reply.as_ref() {
                worker::Reply::Changed { note, .. } => {
                    self.parallel.refresh = true;
                    self.workbench
                        .finish_parallel(id, workbench::Outcome::Completed(note.clone()));
                }
                worker::Reply::FolderExpanded(expansion)
                    if !lane.cancelled && self.transfer_allowed =>
                {
                    self.parallel.refresh = true;
                    if let Err(message) = self.workbench.expand(id, expansion) {
                        self.workbench.finish_parallel(
                            id,
                            workbench::Outcome::Failed {
                                message,
                                partial: false,
                            },
                        );
                    }
                }
                worker::Reply::Conflict(conflict) if !lane.cancelled => {
                    self.workbench.conflict(id);
                    self.parallel
                        .conflicts
                        .push_back((id, ticket, conflict.clone()));
                }
                worker::Reply::TransferSkipped(note)
                    if !lane.cancelled && self.transfer_allowed =>
                {
                    self.status = Some(note.clone());
                    self.workbench
                        .finish_parallel(id, workbench::Outcome::Skipped(note.clone()));
                }
                _ if lane.cancelled => self
                    .workbench
                    .finish_parallel(id, workbench::Outcome::Cancelled),
                _ => self.workbench.finish_parallel(
                    id,
                    workbench::Outcome::Failed {
                        message: "Unexpected SFTP transfer result".into(),
                        partial: false,
                    },
                ),
            },
            Err(mut error) => {
                let partial = error.partial.take();
                let keep_partial =
                    self.transfer_allowed && self.resume_preferences.retention(lane.cancelled);
                if lane.cancelled && !(keep_partial && partial.is_some()) {
                    if self.resume_preferences.enabled
                        && let Some(partial) = partial
                    {
                        partial.discard();
                    }
                    self.workbench
                        .finish_parallel(id, workbench::Outcome::Cancelled);
                } else {
                    let has_partial = if let Some(partial) = partial.filter(|_| keep_partial) {
                        if self.partial.is_none() {
                            self.partial = Some(partial);
                            self.partial_row = Some(id);
                            true
                        } else if self.parallel.offers.len() < usize::from(MAX_PARALLEL) {
                            self.parallel.offers.push_back((Some(id), partial));
                            true
                        } else {
                            false
                        }
                    } else {
                        keep_partial && self.live_partial_ids().contains(&id)
                    };
                    self.workbench.finish_parallel(
                        id,
                        workbench::Outcome::Failed {
                            message: error.message,
                            partial: has_partial,
                        },
                    );
                }
            }
        }
        drop(lane);
        self.schedule_parallel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp_window::{ConflictDefault, EntryKind, EntryName};

    fn window() -> SftpWindow {
        let target = serde_json::from_value(serde_json::json!({
            "host": "parallel.example", "port": 22, "username": "fixture"
        }))
        .unwrap();
        SftpWindow::new(target, None, true, RemoteEditorPolicy::Deny)
    }

    fn transfer(name: &str) -> worker::Transfer {
        worker::Transfer {
            source: Location::Local(name.into()),
            target_directory: Location::Remote(b"/remote".to_vec()),
            name: EntryName::Local(name.into()),
            kind: EntryKind::File,
            title: name.into(),
            size: Some(1),
        }
    }

    #[test]
    fn attached_session_lanes_are_capped_without_changing_the_personal_limit() {
        let mut window = window();
        window.configure_parallel(8, None);
        assert_eq!(window.parallel_limit(), 8);
        window.attached_to = Some(crate::session::SessionId(12));
        assert_eq!(window.parallel_limit(), 2);
        window.configure_parallel(8, Some(1));
        assert_eq!(window.parallel_limit(), 1);
        window.configure_parallel(8, None);
        window.attached_to = None;
        assert_eq!(window.parallel_limit(), 8);
    }

    #[test]
    fn single_transfer_cap_preserves_existing_work_until_running_and_conflict_rows_drain() {
        let mut window = window();
        window.configure_parallel(3, None);
        window.workbench.append(
            VecDeque::from([transfer("first"), transfer("second"), transfer("waiting")]),
            ConflictDefault::Ask,
        );
        let (first, _) = window.workbench.next_at(3).unwrap();
        let (second, _) = window.workbench.next_at(3).unwrap();
        window.workbench.conflict(second);
        window.configure_parallel(3, Some(1));
        assert_eq!(window.parallel.limit, Some(1));
        assert!(!window.parallel_available());
        assert_eq!(window.workbench.active_ids(), [first, second]);
        assert!(window.workbench.next_at(1).is_none());
        window
            .workbench
            .finish_parallel(first, workbench::Outcome::Completed("Done".into()));
        assert!(window.workbench.next_at(1).is_none());
        assert!(window.workbench.resolved(second));
        window
            .workbench
            .finish_parallel(second, workbench::Outcome::Completed("Done".into()));
        assert!(matches!(
            window.workbench.next_at(1).unwrap().1,
            workbench::Pending::Copy(transfer) if transfer.title == "waiting"
        ));
        window.configure_parallel(0, Some(0));
        assert_eq!(window.parallel.limit, Some(1));
        assert_eq!(usize::from(MAX_PARALLEL) + 1, kortty_ssh::MAX_SFTP_LEASES);
    }
}
