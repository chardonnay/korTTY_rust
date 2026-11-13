use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};
use kortty_jobs::{
    DependencyMode, Job, JobGraph, JobId, JobSchedule, RunRequest, RunTrigger, TimeZone,
};

use super::{Copy, Direction, Receipt, Review, RunContext, SaveApproval, nonce, standing};
use crate::scheduler_window::{
    ConnectionRef, Message, Page, RemoteAction, SchedulerView, SchedulerWindow, SshCommand,
    SudoMode, new_id,
};

#[derive(Default)]
pub(in crate::scheduler_window) struct DraftReview {
    pending: Option<Pending>,
    preview: Option<Preview>,
    approved: Option<(Subject, SaveApproval)>,
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::scheduler_window) struct CascadeReview {
    root: JobId,
    graph_revision: u64,
    members: Vec<CascadeMember>,
}

#[derive(Clone, Eq, PartialEq)]
struct CascadeMember {
    job: JobId,
    copy: Option<CascadeCopy>,
}

#[derive(Clone, Eq, PartialEq)]
enum CascadeCopy {
    Available(Receipt),
    NeedsReview,
    Privileged,
    Standing,
}

impl CascadeReview {
    pub(in crate::scheduler_window) fn root(&self) -> &JobId {
        &self.root
    }

    pub(in crate::scheduler_window) fn requires_confirmation(&self) -> bool {
        self.copies().next().is_some()
    }

    pub(in crate::scheduler_window) fn root_unavailable(&self) -> Option<&'static str> {
        let root = self.members.iter().find(|member| member.job == self.root)?;
        match root.copy.as_ref()? {
            CascadeCopy::NeedsReview => Some(
                "The selected SFTP folder copy needs a fresh review before starting its successors.",
            ),
            CascadeCopy::Privileged => Some("Start the privileged folder transfer separately."),
            CascadeCopy::Standing => {
                Some("The recurring SFTP approval is available only to calendar starts.")
            }
            CascadeCopy::Available(_) => None,
        }
    }

    pub(in crate::scheduler_window) fn copies(&self) -> impl Iterator<Item = (&JobId, bool)> {
        self.members
            .iter()
            .filter_map(|member| match member.copy.as_ref() {
                Some(CascadeCopy::Available(_)) => Some((&member.job, true)),
                Some(
                    CascadeCopy::NeedsReview | CascadeCopy::Privileged | CascadeCopy::Standing,
                ) => Some((&member.job, false)),
                None => None,
            })
    }

    pub(in crate::scheduler_window) fn unavailable_reason(
        &self,
        job: &JobId,
    ) -> Option<&'static str> {
        let member = self.members.iter().find(|member| member.job == *job)?;
        match member.copy.as_ref()? {
            CascadeCopy::Standing => Some(
                "The recurring approval is available only to calendar starts. This branch stops if it reaches this job.",
            ),
            _ => None,
        }
    }

    fn receipt(&self, revision: u64, root: &JobId, job: &JobId) -> Option<&Receipt> {
        if self.graph_revision != revision || self.root != *root {
            return None;
        }
        let member = self.members.iter().find(|member| member.job == *job)?;
        match member.copy.as_ref()? {
            CascadeCopy::Available(receipt) => Some(receipt),
            CascadeCopy::NeedsReview | CascadeCopy::Privileged | CascadeCopy::Standing => None,
        }
    }
}

struct Pending {
    ticket: String,
    subject: Subject,
    expires_at: Option<SystemTime>,
    cancelled: Arc<AtomicBool>,
}

struct Preview {
    subject: Subject,
    review: Arc<Review>,
    expires_at: Option<SystemTime>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Subject {
    id: JobId,
    scope: [u8; 32],
    schedule: String,
    mode: DependencyMode,
    predecessors: Vec<JobId>,
    max_age_minutes: Option<NonZeroU32>,
    sudo: SudoMode,
    direction: Direction,
    replace_changed: bool,
    standing: Option<standing::Settings>,
}

pub(crate) struct PreviewRequest {
    pub(crate) ticket: String,
    pub(crate) copy: Copy,
    pub(crate) connection: ConnectionRef,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) sudo: SudoMode,
}

impl SchedulerWindow {
    pub(in crate::scheduler_window) fn capture_sync_cascade(
        &self,
        root: &JobId,
        members: &[&Job<SshCommand>],
    ) -> CascadeReview {
        let has_one_use_copy = members.iter().any(|job| {
            matches!(job.action().remote.as_ref(), Some(RemoteAction::Sync(copy)) if copy.standing.is_none())
        });
        let mut receipts = if has_one_use_copy {
            self.sync_store.snapshot().unwrap_or_default()
        } else {
            Default::default()
        };
        let members = members
            .iter()
            .map(|job| {
                let copy = match &job.action().remote {
                    Some(RemoteAction::Sync(copy)) if copy.standing.is_some() => {
                        Some(CascadeCopy::Standing)
                    }
                    Some(RemoteAction::Sync(_)) => {
                        let receipt =
                            subject(job, &self.zone)
                                .ok()
                                .and_then(|(subject, copy, _)| {
                                    receipts.remove(job.id()).filter(|receipt| {
                                        receipt.matches_approval(
                                            copy.approval.as_deref(),
                                            subject.scope,
                                        )
                                    })
                                });
                        Some(receipt.map_or(CascadeCopy::NeedsReview, CascadeCopy::Available))
                    }
                    Some(
                        RemoteAction::PrivilegedTreeUpload(_)
                        | RemoteAction::PrivilegedTreeDownload(_),
                    ) => Some(CascadeCopy::Privileged),
                    _ => None,
                };
                CascadeMember {
                    job: job.id().clone(),
                    copy,
                }
            })
            .collect();
        CascadeReview {
            root: root.clone(),
            graph_revision: self.graph_revision,
            members,
        }
    }

    pub(crate) fn begin_sync_review(
        &self,
        view: &mut SchedulerView,
    ) -> Result<PreviewRequest, String> {
        if view.page != Page::Action
            || view.confirm.is_some()
            || !self.can_browse_directory()
            || !self.can_edit()
            || !self.draft_matches(view)
        {
            return Err("The selected job cannot be reviewed right now.".into());
        }
        let id = view.draft.id.clone().map_or_else(new_id, Ok)?;
        let job = self.build_current_draft(view, id)?;
        let (subject, copy, connection) = subject(&job, &self.zone)?;
        let sudo = subject.sudo;
        let expires_at = subject
            .standing
            .map(|settings| settings.expiry(SystemTime::now()))
            .transpose()
            .map_err(|error| error.to_string())?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let ticket = nonce().map_err(|_| "Could not allocate a folder review.")?;
        view.sync_review = DraftReview {
            pending: Some(Pending {
                ticket: ticket.clone(),
                subject,
                expires_at,
                cancelled: Arc::clone(&cancelled),
            }),
            ..DraftReview::default()
        };
        view.status = None;
        Ok(PreviewRequest {
            ticket,
            copy: copy.clone(),
            connection: connection.clone(),
            cancelled,
            sudo,
        })
    }

    pub(in crate::scheduler_window) fn approve_sync(
        &self,
        view: &mut SchedulerView,
    ) -> Result<(), String> {
        let Some(Preview {
            subject: approved,
            review,
            expires_at,
        }) = view.sync_review.preview.take()
        else {
            return Ok(());
        };
        if view.page != Page::Action
            || !self.can_browse_directory()
            || !self.can_edit()
            || !self.draft_matches(view)
            || subject(
                &self.build_current_draft(view, approved.id.clone())?,
                &self.zone,
            )?
            .0 != approved
        {
            return Err("The reviewed job settings changed. Compare the folders again.".into());
        }
        let (approval, status) = if let Some(settings) = approved.standing {
            let anchors = review
                .standing_roots
                .ok_or("Review the recurring folder roots again.")?;
            let expires_at = expires_at.ok_or("Review the recurring folder expiry again.")?;
            let approval = standing::Approval::new(
                approved.scope,
                review.pins.clone(),
                anchors,
                expires_at,
                settings.limits,
            )
            .map_err(|error| error.to_string())?;
            (
                SaveApproval::Standing(approval),
                "Recurring folder copy approved. Save the job to keep the calendar approval.",
            )
        } else {
            let receipt = Receipt::new(approved.scope, &review)
                .map_err(|_| "Could not allocate the saved SFTP approval.")?;
            (
                SaveApproval::OneUse(receipt),
                "Folder copy approved. Save the job to keep this one-use approval.",
            )
        };
        view.sync_review.approved = Some((approved, approval));
        view.dirty = true;
        view.status = Some(status.into());
        Ok(())
    }

    pub(crate) fn sync_run_context(&self, request: &RunRequest<SshCommand>) -> Option<RunContext> {
        if !self.execution_allowed
            || self.draining
            || request.cancellation.is_requested()
            || !self.active.get(&request.id).is_some_and(|active| {
                active.job == request.job
                    && active.started_at == request.started_at
                    && active.trigger == request.trigger
                    && active
                        .cancellation
                        .shares_signal_with(&request.cancellation)
            })
        {
            return None;
        }
        let job = self.scheduler.graph().job(&request.job)?;
        if !std::ptr::eq(job.action(), request.action.as_ref()) {
            return None;
        }
        let (subject, copy, _) = subject(job, &self.zone).ok()?;
        let now = SystemTime::now();
        let unblocked = || {
            self.scheduler
                .blockers(&request.job, now)
                .is_ok_and(|blockers| blockers.is_empty())
        };
        let (standing, cascade_receipt) = match (subject.standing, request.trigger) {
            (Some(_), RunTrigger::Scheduled) if self.can_run() && unblocked() => {
                let firing = standing::Firing {
                    epoch: self.sync_epoch?,
                    run: request.id,
                };
                let approval = self
                    .sync_store
                    .load_standing(&request.job, copy.approval.as_deref(), subject.scope, now)
                    .ok()?;
                if !approval.active_at(request.started_at) {
                    return None;
                }
                (Some((approval, firing)), None)
            }
            (Some(_), _) => return None,
            (None, RunTrigger::Manual | RunTrigger::Scheduled) => (None, None),
            (None, RunTrigger::Dependency)
                if subject.sudo == SudoMode::Disabled
                    && subject.mode == DependencyMode::Chain
                    && self.can_run()
                    && unblocked() =>
            {
                (None, None)
            }
            (None, RunTrigger::Cascade) if subject.sudo == SudoMode::Disabled && self.can_run() => {
                let origin = self.scheduler.active_cascade_origin(request)?;
                let (review, accepted_blockers) = self.sync_cascades.get(&origin.root_run)?;
                let still_eligible = if origin.is_root {
                    self.cascade_root_conditions_match(&request.job, accepted_blockers, now)
                } else {
                    self.scheduler
                        .active_cascade_blockers(request, now)
                        .is_some_and(|blockers| blockers.is_empty())
                };
                if !still_eligible {
                    return None;
                }
                let captured =
                    review.receipt(self.graph_revision, origin.root_job, &request.job)?;
                let current = self
                    .sync_store
                    .load(&request.job, copy.approval.as_deref(), subject.scope)
                    .ok()?;
                if captured != &current {
                    return None;
                }
                (None, Some(current))
            }
            (None, RunTrigger::Dependency | RunTrigger::Cascade) => return None,
        };
        Some(RunContext {
            store: self.sync_store.clone(),
            scope: subject.scope,
            run: request.id,
            job: request.job.clone(),
            action: Arc::clone(&request.action),
            started_at: request.started_at,
            trigger: request.trigger,
            cancellation: request.cancellation.clone(),
            cascade_receipt,
            standing,
        })
    }

    pub(in crate::scheduler_window) fn sync_save(
        &self,
        view: &SchedulerView,
        job: Job<SshCommand>,
    ) -> Result<(Job<SshCommand>, Option<SaveApproval>), String> {
        let (current, copy, _) = match subject(&job, &self.zone) {
            Ok(subject) => subject,
            Err(error) if matches!(&job.action().remote, Some(RemoteAction::Sync(copy)) if copy.standing.is_some()) =>
            {
                return Err(error);
            }
            Err(_) => return Ok((job, None)),
        };
        let mut action = job.action().clone();
        let Some(
            RemoteAction::Sync(next)
            | RemoteAction::PrivilegedTreeUpload(next)
            | RemoteAction::PrivilegedTreeDownload(next),
        ) = &mut action.remote
        else {
            return Ok((job, None));
        };
        let mut replacement_receipt = None;
        if let Some((approved, approval)) = &view.sync_review.approved {
            if &current != approved {
                return Err("The approved job settings changed. Compare the folders again.".into());
            }
            next.approval = Some(approval.nonce().to_owned());
            replacement_receipt = Some(approval.clone());
        } else {
            let approval = copy.approval.as_deref();
            let is_current = if current.standing.is_some() {
                self.sync_store
                    .load_standing(&current.id, approval, current.scope, SystemTime::now())
                    .is_ok()
            } else {
                self.sync_store
                    .load(&current.id, approval, current.scope)
                    .is_ok()
            };
            if !is_current {
                next.approval = None;
            }
        }
        job.definition()
            .with_action(action)
            .map(|job| (job, replacement_receipt))
            .map_err(|error| error.to_string())
    }
}

fn subject<'a>(
    job: &'a Job<SshCommand>,
    zone: &TimeZone,
) -> Result<(Subject, &'a Copy, &'a ConnectionRef), String> {
    let action = job.action();
    let (copy, sudo) = match &action.remote {
        Some(RemoteAction::Sync(copy)) => (copy, SudoMode::Disabled),
        Some(
            RemoteAction::PrivilegedTreeUpload(copy) | RemoteAction::PrivilegedTreeDownload(copy),
        ) if action.sudo != SudoMode::Disabled => (copy, action.sudo),
        _ => return Err("Choose a reviewed folder copy action.".into()),
    };
    let connection = action
        .connection
        .as_ref()
        .ok_or("Choose one saved SSH connection.")?;
    if !action.additional_connections.is_empty() {
        return Err("Reviewed SFTP copy requires one saved connection.".into());
    }
    let schedule = job.schedule();
    let privileged = sudo != SudoMode::Disabled;
    let predecessors = job.dependencies();
    let max_age_minutes = job.max_age_minutes().filter(|_| !predecessors.is_empty());
    copy.validate_job_schedule(schedule, job.mode(), predecessors, privileged)?;
    let scope = if copy.standing.is_some() {
        copy.standing_scope(job, connection, zone)
            .map_err(|error| error.to_string())?
    } else {
        if privileged {
            copy.privileged_scope(connection, schedule, sudo)
        } else {
            copy.conditioned_scope(
                connection,
                schedule,
                job.mode(),
                predecessors,
                max_age_minutes,
            )
        }
        .map_err(|_| "Could not bind this review to the job.")?
    };
    let schedule = if copy.standing.is_some() {
        standing_schedule(
            schedule.ok_or("Choose an enabled calendar for recurring SFTP copies.")?,
            zone,
        )
    } else {
        schedule.filter(|schedule| schedule.enabled()).map_or_else(
            || "Run Now".to_owned(),
            |schedule| {
                let (date, _) = schedule.active_dates();
                let time = schedule.fixed_times().next();
                match (date, time) {
                    (Some(date), Some(time)) => {
                        format!("Run Now or {date} at {time} (scheduler time zone)")
                    }
                    _ => "Run Now".to_owned(),
                }
            },
        )
    };
    Ok((
        Subject {
            id: job.id().clone(),
            scope,
            schedule,
            mode: job.mode(),
            predecessors: predecessors.to_vec(),
            max_age_minutes,
            sudo,
            direction: copy.direction,
            replace_changed: copy.replace_changed,
            standing: copy.standing,
        },
        copy,
        connection,
    ))
}

fn standing_schedule(schedule: &JobSchedule, zone: &TimeZone) -> String {
    let (from, until) = schedule.active_dates();
    let (start, end) = schedule.window();
    let weekdays = schedule
        .weekdays()
        .map(|day| format!("{day:?}"))
        .collect::<Vec<_>>();
    let weekdays = if weekdays.is_empty() {
        "Every day".into()
    } else {
        weekdays.join(", ")
    };
    let times = schedule
        .fixed_times()
        .map(|time| time.to_string())
        .collect::<Vec<_>>();
    let mut triggers = Vec::new();
    if !times.is_empty() {
        triggers.push(format!("Fixed times · {}", times.join(", ")));
    }
    if let Some(interval) = schedule.interval_minutes() {
        triggers.push(format!("Every {interval} min from the window start"));
    }
    if triggers.is_empty() {
        triggers.push("At the window start".into());
    }
    let dates = match (from, until) {
        (Some(from), Some(until)) => format!("{from} through {until}"),
        (Some(from), None) => format!("From {from}"),
        (None, Some(until)) => format!("Through {until}"),
        (None, None) => "All calendar dates".into(),
    };
    let zone = zone.iana_name().unwrap_or("Scheduler time zone");
    format!(
        "Calendar only · {zone}\n{dates} · {weekdays}\nWindow · {start}–{end}\n{}",
        triggers.join("\n")
    )
}

impl DraftReview {
    pub(in crate::scheduler_window) fn approved_job(&self) -> Option<&JobId> {
        self.approved.as_ref().map(|(subject, _)| &subject.id)
    }

    pub(in crate::scheduler_window) fn finish(
        &mut self,
        ticket: &str,
        result: Result<Arc<Review>, String>,
    ) -> Option<String> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.ticket != ticket)
        {
            return None;
        }
        let pending = self.pending.take()?;
        match result {
            Ok(review) => {
                let standing = pending.subject.standing.is_some();
                if standing != review.standing_roots.is_some()
                    || !pending.subject.replace_changed && !review.preview.replacements.is_empty()
                {
                    return Some("The folder review returned a different approval type. Compare the folders again.".into());
                }
                self.preview = Some(Preview {
                    subject: pending.subject.clone(),
                    review,
                    expires_at: pending.expires_at,
                });
                Some(if standing {
                    "Review the recurring calendar, folder roots and transfer limits before approving."
                } else {
                    "Review the exact folder-copy actions before approving."
                }.into())
            }
            Err(error) => Some(error),
        }
    }

    pub(in crate::scheduler_window) fn view(
        &self,
        graph: &JobGraph<SshCommand>,
    ) -> Option<Element<'_, Message>> {
        if self.pending.is_some() {
            return Some(
                row![
                    text("Comparing folders and verifying source content…").width(Length::Fill),
                    button("Cancel review").on_press(Message::DiscardSync),
                ]
                .spacing(8)
                .into(),
            );
        }
        let Preview {
            subject,
            review,
            expires_at,
        } = self.preview.as_ref()?;
        let [source, target] = &review.preview.roots;
        let privileged = subject.sudo != SudoMode::Disabled;
        let standing = subject.standing;
        let mut details = column![
            text(match (privileged, subject.direction, standing.is_some()) {
                (true, Direction::Upload, _) => "One-use privileged folder upload review",
                (true, Direction::Download, _) => "One-use privileged folder download review",
                (_, _, true) => "Recurring SFTP copy review",
                _ if subject.replace_changed => "One-use SFTP download and replacement review",
                _ => "One-use SFTP copy review",
            })
            .size(17),
            text(format!("Source · {source}")),
            text(format!("Destination · {target}")),
            text(&subject.schedule).size(13),
            text(format!(
                "{}{} actions · {} bytes",
                if standing.is_some() {
                    "Current comparison · "
                } else {
                    ""
                },
                review.preview.paths.len() + review.preview.replacements.len(),
                review.preview.bytes
            )),
        ]
        .spacing(5);
        if !privileged && matches!(subject.direction, Direction::Download) {
            details = details.push(text("New local files and folders use normal destination permissions and inherited access. New files use their local write time.").size(12));
        }
        if !review.preview.replacements.is_empty() {
            details = details.push(text("Replacement preserves existing local access permissions and uses the source timestamp. Each original remains in a private sibling folder. The run result gives the shared recovery pattern.").size(12));
        }
        if !subject.predecessors.is_empty() {
            if subject.mode == DependencyMode::Chain {
                details = details
                    .push(text("Chain can start the copy when every predecessor has succeeded since this job last ran.").size(12))
                    .push(text("A Chain start can occur outside the optional calendar date and time.").size(12))
                    .push(text("Run Now, a calendar start and a Chain start share this approval. The approval is used before the reviewed copy writes.").size(12));
            }
            details = details.push(
                text("A calendar start requires the last result of each predecessor to be successful.")
                    .size(12),
            );
            for id in &subject.predecessors {
                let label = graph
                    .job(id)
                    .map_or_else(|| id.to_string(), |job| format!("{} · {id}", job.name()));
                details = details.push(text(label).size(12));
            }
            let freshness = subject.max_age_minutes.map_or_else(
                || "No maximum age for successful predecessors.".to_owned(),
                |age| format!("Maximum age of a predecessor success · {age} min"),
            );
            details = details.push(text(freshness).size(12));
            if standing.is_none() {
                details = details.push(text("Run Now and Run with successors can bypass unmet conditions for the selected root after confirmation.").size(12));
            }
        }
        if let Some(settings) = standing {
            let expires = expires_at
                .and_then(|time| jiff::Timestamp::try_from(time).ok())
                .map_or_else(|| "Unavailable".into(), |time| time.to_string());
            details = details
                .push(text(format!("Approval expires · {expires}")).size(12))
                .push(text(format!(
                    "Up to {} calendar attempts · {} actions and {} bytes per attempt",
                    settings.limits.max_firings, settings.limits.max_actions, settings.limits.max_bytes,
                )).size(12))
                .push(text("The approval covers future eligible source content under these folder roots. It allows creation of missing regular files and folders only.").size(12))
                .push(text("The calendar window selects due times. A queued or overdue run can start later while the approval is active.").size(12))
                .push(text("An admitted calendar attempt uses one allowance, including a failed connection or an empty copy plan.").size(12));
        } else if privileged {
            details = details
                .push(
                    text(match subject.direction {
                        Direction::Upload => {
                            "Publishes one new root-owned folder with private permissions."
                        }
                        Direction::Download => {
                            "Publishes one new local folder with private permissions."
                        }
                    })
                    .size(12),
                )
                .push(
                    text(match subject.sudo {
                        SudoMode::Vault => {
                            "Sudo authentication uses the exact saved vault credential."
                        }
                        _ => "The route must allow passwordless sudo.",
                    })
                    .size(12),
                );
        } else {
            details = details.push(text("Run with successors needs its own start confirmation to use this saved approval. The first eligible execution to claim the approval uses it.").size(12));
        }
        for pin in &review.pins {
            details = details.push(text(pin).size(12));
        }
        for (directory, path) in &review.preview.paths {
            let verb = if *directory {
                "Create folder"
            } else {
                "Copy new file"
            };
            details = details.push(text(format!("{verb} · {path}")).font(iced::Font::MONOSPACE));
        }
        for replacement in &review.preview.replacements {
            details = details.push(
                column![
                    text(format!("Replace local file · {}", replacement.target))
                        .font(iced::Font::MONOSPACE),
                    text(format!("Source · {}", replacement.source)).size(12),
                    text(format!("Current SHA-256 · {}", replacement.target_sha256)).size(12),
                    text(format!("Incoming SHA-256 · {}", replacement.source_sha256)).size(12),
                ]
                .spacing(3),
            );
        }
        Some(
            column![
                scrollable(details).height(Length::Fixed(225.0)),
                row![
                    button("Discard review").on_press(Message::DiscardSync),
                    button(if standing.is_some() {
                        "Approve recurring runs"
                    } else {
                        "Approve one run"
                    })
                    .on_press(Message::ApproveSync),
                ]
                .spacing(8),
            ]
            .spacing(8)
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_review_binds_an_available_receipt_to_its_root_revision_and_member() {
        let root = JobId::new("prepare").unwrap();
        let ready = JobId::new("ready-copy").unwrap();
        let missing = JobId::new("missing-copy").unwrap();
        let privileged = JobId::new("privileged-copy").unwrap();
        let outside = JobId::new("outside-copy").unwrap();
        let receipt =
            Receipt::for_operation([1; 32], [2; 32], vec!["pinned route".into()]).unwrap();
        let review = CascadeReview {
            root: root.clone(),
            graph_revision: 3,
            members: vec![
                CascadeMember {
                    job: root.clone(),
                    copy: None,
                },
                CascadeMember {
                    job: ready.clone(),
                    copy: Some(CascadeCopy::Available(receipt.clone())),
                },
                CascadeMember {
                    job: missing.clone(),
                    copy: Some(CascadeCopy::NeedsReview),
                },
                CascadeMember {
                    job: privileged.clone(),
                    copy: Some(CascadeCopy::Privileged),
                },
            ],
        };
        assert_eq!(review.root(), &root);
        assert!(review.requires_confirmation());
        assert!(review.root_unavailable().is_none());
        assert_eq!(
            review.copies().collect::<Vec<_>>(),
            [(&ready, true), (&missing, false), (&privileged, false)]
        );
        assert!(review.receipt(3, &root, &ready) == Some(&receipt));
        assert!(review.receipt(3, &root, &missing).is_none());
        assert!(review.receipt(3, &root, &privileged).is_none());
        assert!(review.receipt(3, &root, &outside).is_none());
        assert!(review.receipt(4, &root, &ready).is_none());
        assert!(review.receipt(3, &outside, &ready).is_none());
    }
}
