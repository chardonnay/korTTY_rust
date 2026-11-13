//! Process-local authority for a target and round in an active scheduled Agent firing.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use kortty_jobs::{RunId, RunRequest, RunTrigger};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Key {
    run: RunId,
    target_index: usize,
}

impl Key {
    pub(crate) fn new(run: RunId, target_index: usize) -> Self {
        Self { run, target_index }
    }

    pub(crate) fn run(self) -> RunId {
        self.run
    }

    pub(crate) fn target_index(self) -> usize {
        self.target_index
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ticket {
    key: Key,
    round: u8,
    nonce: String,
}

impl Ticket {
    pub(crate) fn key(&self) -> Key {
        self.key
    }

    pub(crate) fn run(&self) -> RunId {
        self.key.run()
    }

    pub(crate) fn target_index(&self) -> usize {
        self.key.target_index()
    }

    pub(crate) fn round(&self) -> u8 {
        self.round
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Generating,
    AwaitingReview,
    Authorized,
    Executing,
}

pub(crate) struct Review<A, C, P> {
    request: Arc<RunRequest<A>>,
    ticket: Ticket,
    context: C,
    deadline: Instant,
    phase: Phase,
    proposal: Option<P>,
}

impl<A, C, P> Review<A, C, P> {
    pub(crate) fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    pub(crate) fn request(&self) -> &Arc<RunRequest<A>> {
        &self.request
    }

    pub(crate) fn context(&self) -> &C {
        &self.context
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        (self.phase != Phase::Executing).then_some(self.deadline)
    }

    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    pub(crate) fn proposal(&self) -> Option<&P> {
        self.proposal.as_ref()
    }

    fn inactive(&self, now: Instant) -> Option<EndReason> {
        if self.request.cancellation.is_requested() {
            Some(EndReason::Cancelled)
        } else if self.deadline().is_some_and(|deadline| now >= deadline) {
            Some(EndReason::Expired)
        } else {
            None
        }
    }
}

pub(crate) struct Execution<A, P> {
    pub(crate) request: Arc<RunRequest<A>>,
    pub(crate) proposal: P,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EndReason {
    Cancelled,
    Expired,
}

pub(crate) struct Ended {
    pub(crate) ticket: Ticket,
    pub(crate) phase: Phase,
    pub(crate) reason: EndReason,
}

pub(crate) struct Reviews<A, C, P> {
    entries: BTreeMap<Key, Review<A, C, P>>,
}

impl<A, C, P> Default for Reviews<A, C, P> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<A, C, P> Reviews<A, C, P> {
    pub(crate) fn begin(
        &mut self,
        request: Arc<RunRequest<A>>,
        target_index: usize,
        round: u8,
        context: C,
        now: Instant,
        deadline: Instant,
    ) -> Result<Ticket, &'static str> {
        if round == 0
            || request.cancellation.is_requested()
            || !matches!(request.trigger, RunTrigger::Manual | RunTrigger::Scheduled)
        {
            return Err("The scheduled Agent firing cannot begin a proposal.");
        }
        if deadline <= now {
            return Err("The scheduled Agent proposal deadline has passed.");
        }
        let key = Key::new(request.id, target_index);
        if self.entries.contains_key(&key) {
            return Err("The scheduled Agent target already has a proposal.");
        }
        if self
            .for_run(request.id)
            .next()
            .is_some_and(|review| !Arc::ptr_eq(review.request(), &request))
        {
            return Err("The scheduled Agent firing changed.");
        }
        let ticket = Ticket {
            key,
            round,
            nonce: crate::private_files::nonce()
                .map_err(|_| "Could not identify the scheduled Agent proposal.")?,
        };
        self.entries.insert(
            key,
            Review {
                request,
                ticket: ticket.clone(),
                context,
                deadline,
                phase: Phase::Generating,
                proposal: None,
            },
        );
        Ok(ticket)
    }

    pub(crate) fn by_key(&self, key: Key) -> Option<&Review<A, C, P>> {
        self.entries.get(&key)
    }

    pub(crate) fn by_ticket(&self, ticket: &Ticket) -> Option<&Review<A, C, P>> {
        self.by_key(ticket.key())
            .filter(|review| review.ticket() == ticket)
    }

    pub(crate) fn for_run(&self, run: RunId) -> impl Iterator<Item = &Review<A, C, P>> {
        self.entries
            .range(Key::new(run, 0)..=Key::new(run, usize::MAX))
            .map(|(_, review)| review)
    }

    pub(crate) fn begin_preapproved_probe(
        &mut self,
        request: Arc<RunRequest<A>>,
        target_index: usize,
        context: C,
        proposal: P,
        now: Instant,
        deadline: Instant,
    ) -> Result<Ticket, &'static str> {
        let ticket = self.begin(request, target_index, 1, context, now, deadline)?;
        let review = self
            .entries
            .get_mut(&ticket.key())
            .expect("new probe review");
        review.proposal = Some(proposal);
        review.phase = Phase::Authorized;
        Ok(ticket)
    }

    pub(crate) fn get(&self, ticket: &Ticket, now: Instant) -> Option<&Review<A, C, P>> {
        self.by_ticket(ticket)
            .filter(|review| review.inactive(now).is_none())
    }

    fn pending(
        &mut self,
        ticket: &Ticket,
        phase: Phase,
        now: Instant,
    ) -> Option<&mut Review<A, C, P>> {
        self.entries.get_mut(&ticket.key()).filter(|review| {
            &review.ticket == ticket && review.phase == phase && review.inactive(now).is_none()
        })
    }

    pub(crate) fn publish(
        &mut self,
        ticket: &Ticket,
        proposal: P,
        now: Instant,
        review_deadline: Instant,
    ) -> bool {
        if review_deadline <= now {
            return false;
        }
        let Some(review) = self.pending(ticket, Phase::Generating, now) else {
            return false;
        };
        review.deadline = review_deadline;
        review.proposal = Some(proposal);
        review.phase = Phase::AwaitingReview;
        true
    }

    pub(crate) fn authorize(&mut self, ticket: &Ticket, now: Instant) -> bool {
        let Some(review) = self.pending(ticket, Phase::AwaitingReview, now) else {
            return false;
        };
        review.phase = Phase::Authorized;
        true
    }

    pub(crate) fn claim(&mut self, ticket: &Ticket, now: Instant) -> Option<Execution<A, P>> {
        let review = self.pending(ticket, Phase::Authorized, now)?;
        let proposal = review.proposal.take()?;
        review.phase = Phase::Executing;
        Some(Execution {
            request: Arc::clone(&review.request),
            proposal,
        })
    }

    /// Consume a terminal event for exactly the phase that issued it.
    pub(crate) fn settle(
        &mut self,
        ticket: &Ticket,
        expected: Phase,
    ) -> Option<Arc<RunRequest<A>>> {
        if self.by_ticket(ticket)?.phase != expected {
            return None;
        }
        self.entries
            .remove(&ticket.key())
            .map(|review| review.request)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.entries.values().filter_map(Review::deadline).min()
    }

    pub(crate) fn next_authorized(
        &self,
        now: Instant,
        admitted: impl Fn(Key) -> bool,
    ) -> Option<Ticket> {
        self.entries.values().find_map(|review| {
            (review.phase == Phase::Authorized
                && review.inactive(now).is_none()
                && admitted(review.ticket.key()))
            .then(|| review.ticket.clone())
        })
    }

    pub(crate) fn generating_count(&self) -> usize {
        self.entries
            .values()
            .filter(|review| review.phase == Phase::Generating)
            .count()
    }

    /// Executing work remains active until its worker reports completion.
    pub(crate) fn settle_inactive(&mut self, now: Instant) -> Vec<Ended> {
        let mut ended = Vec::new();
        self.entries.retain(|_, review| {
            if review.phase != Phase::Executing
                && let Some(reason) = review.inactive(now)
            {
                ended.push(Ended {
                    ticket: review.ticket.clone(),
                    phase: review.phase,
                    reason,
                });
                return false;
            }
            true
        });
        ended
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use kortty_jobs::{Job, JobGraph, JobId, RunOutcome, Scheduler};

    use super::*;

    const WINDOW: Duration = Duration::from_secs(15);

    fn scheduler() -> (Scheduler<()>, JobId) {
        let job = JobId::new("review-fixture").unwrap();
        let graph =
            JobGraph::new(vec![Job::new(job.clone(), "Review fixture", ()).unwrap()]).unwrap();
        (Scheduler::new(graph), job)
    }

    fn firing(scheduler: &mut Scheduler<()>, job: &JobId) -> Arc<RunRequest<()>> {
        Arc::new(
            scheduler
                .start_manual(job, SystemTime::now())
                .unwrap()
                .started
                .pop()
                .unwrap(),
        )
    }

    #[test]
    fn scheduled_agent_review_requires_the_exact_ticket_and_consumes_the_proposal_once() {
        let (mut scheduler, job) = scheduler();
        let request = firing(&mut scheduler, &job);
        let now = Instant::now();
        let mut reviews = Reviews::default();
        let ticket = reviews
            .begin(
                Arc::clone(&request),
                0,
                1,
                "reviewed context",
                now,
                now + WINDOW,
            )
            .unwrap();
        assert_eq!(ticket.run(), request.id);
        assert_eq!(reviews.next_deadline(), Some(now + WINDOW));
        assert_eq!(
            reviews.get(&ticket, now).unwrap().phase(),
            Phase::Generating
        );
        assert!(
            reviews
                .begin(
                    Arc::clone(&request),
                    0,
                    1,
                    "other context",
                    now,
                    now + WINDOW
                )
                .is_err()
        );
        let mut wrong = ticket.clone();
        wrong.nonce.push('0');
        assert!(reviews.get(&wrong, now).is_none());
        assert!(reviews.settle(&wrong, Phase::Generating).is_none());
        assert!(!reviews.publish(&wrong, ["wrong"], now, now + WINDOW));
        assert!(reviews.claim(&ticket, now).is_none());
        assert!(!reviews.authorize(&ticket, now));

        let proposed = ["exact command"];
        assert!(reviews.publish(&ticket, proposed, now, now + WINDOW));
        assert!(!reviews.publish(&ticket, ["late result"], now, now + WINDOW));
        let review = reviews.by_key(ticket.key()).unwrap();
        assert_eq!(review.phase(), Phase::AwaitingReview);
        assert_eq!(review.ticket(), &ticket);
        assert_eq!(review.context(), &"reviewed context");
        assert_eq!(review.proposal(), Some(&proposed));
        assert!(Arc::ptr_eq(review.request(), &request));
        assert!(!reviews.authorize(&wrong, now));
        assert!(reviews.authorize(&ticket, now));
        assert!(!reviews.authorize(&ticket, now));
        assert!(reviews.settle(&ticket, Phase::AwaitingReview).is_none());

        let execution = reviews.claim(&ticket, now).unwrap();
        assert!(Arc::ptr_eq(&execution.request, &request));
        assert_eq!(execution.proposal, proposed);
        assert!(reviews.claim(&ticket, now).is_none());
        assert!(reviews.next_deadline().is_none());
        assert!(reviews.settle_inactive(now + WINDOW).is_empty());
        let executing = reviews.get(&ticket, now + WINDOW).unwrap();
        assert_eq!(executing.phase(), Phase::Executing);
        assert!(executing.proposal().is_none());
        assert!(reviews.settle(&ticket, Phase::Generating).is_none());
        assert!(reviews.settle(&ticket, Phase::Executing).is_some());
        assert!(reviews.settle(&ticket, Phase::Executing).is_none());
    }

    #[test]
    fn scheduled_agent_review_rejects_expiry_at_each_phase_before_ssh() {
        for phase in [Phase::Generating, Phase::AwaitingReview, Phase::Authorized] {
            let (mut scheduler, job) = scheduler();
            let request = firing(&mut scheduler, &job);
            let now = Instant::now();
            let deadline = now + WINDOW;
            let mut reviews = Reviews::default();
            assert!(
                reviews
                    .begin(Arc::clone(&request), 0, 1, (), now, now)
                    .is_err()
            );
            let ticket = reviews.begin(request, 0, 1, (), now, deadline).unwrap();
            if phase != Phase::Generating {
                assert!(reviews.publish(&ticket, "command", now, deadline));
            }
            if phase == Phase::Authorized {
                assert!(reviews.authorize(&ticket, now));
            }
            assert!(reviews.get(&ticket, deadline).is_none());
            assert!(!reviews.publish(&ticket, "late result", deadline, deadline + WINDOW));
            assert!(!reviews.authorize(&ticket, deadline));
            assert!(reviews.claim(&ticket, deadline).is_none());
            let ended = reviews.settle_inactive(deadline);
            assert_eq!(ended.len(), 1);
            assert_eq!(ended[0].ticket, ticket);
            assert_eq!(ended[0].phase, phase);
            assert_eq!(ended[0].reason, EndReason::Expired);
            assert!(reviews.settle_inactive(deadline).is_empty());
            assert!(reviews.settle(&ticket, phase).is_none());
        }
    }

    #[test]
    fn scheduled_agent_review_cancellation_cannot_authorize_the_next_firing() {
        let (mut scheduler, job) = scheduler();
        let first = firing(&mut scheduler, &job);
        let now = Instant::now();
        let mut reviews = Reviews::default();
        let ticket = reviews
            .begin(Arc::clone(&first), 0, 1, (), now, now + WINDOW)
            .unwrap();
        assert!(reviews.publish(&ticket, "first", now, now + WINDOW));
        scheduler.cancel(first.id).unwrap();
        assert!(!reviews.authorize(&ticket, now));
        let ended = reviews.settle_inactive(now);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].reason, EndReason::Cancelled);
        assert!(
            reviews
                .begin(Arc::clone(&first), 0, 1, (), now, now + WINDOW)
                .is_err()
        );
        let completed = scheduler
            .complete(first.id, RunOutcome::Success, SystemTime::now())
            .unwrap();
        assert_eq!(completed.finished[0].outcome, RunOutcome::Cancelled);

        let second = firing(&mut scheduler, &job);
        let next = reviews
            .begin(Arc::clone(&second), 0, 1, (), now, now + WINDOW)
            .unwrap();
        assert_ne!(ticket, next);
        assert!(!reviews.publish(&ticket, "old proposal", now, now + WINDOW));
        assert!(reviews.publish(&next, "second", now, now + WINDOW));
        assert!(!reviews.authorize(&ticket, now));
        assert!(reviews.authorize(&next, now));
        assert!(reviews.claim(&next, now).is_some());
        scheduler.cancel(second.id).unwrap();
        assert!(reviews.get(&next, now).is_none());
        assert!(reviews.settle_inactive(now).is_empty());
        assert!(reviews.settle(&next, Phase::Executing).is_some());
    }

    #[test]
    fn scheduled_target_reviews_keep_independent_tickets_and_rounds() {
        let (mut scheduler, job) = scheduler();
        let request = firing(&mut scheduler, &job);
        let now = Instant::now();
        let deadline = now + WINDOW;
        let mut reviews = Reviews::default();
        let second = reviews
            .begin(Arc::clone(&request), 1, 1, "second", now, deadline)
            .unwrap();
        let first = reviews
            .begin_preapproved_probe(Arc::clone(&request), 0, "first", "probe", now, deadline)
            .unwrap();
        assert_eq!(first.key(), Key::new(request.id, 0));
        assert_eq!(first.round(), 1);
        assert_eq!(reviews.generating_count(), 1);
        let positions: Vec<_> = reviews
            .for_run(request.id)
            .map(|review| review.ticket().target_index())
            .collect();
        assert_eq!(positions, [0, 1]);

        let unrelated = Arc::new(RunRequest {
            id: request.id,
            job: request.job.clone(),
            action: Arc::clone(&request.action),
            trigger: request.trigger,
            started_at: request.started_at,
            cancellation: Default::default(),
        });
        assert!(
            reviews
                .begin(unrelated, 2, 1, "wrong run", now, deadline)
                .is_err()
        );
        assert!(
            reviews
                .begin(Arc::clone(&request), 2, 0, "invalid round", now, deadline)
                .is_err()
        );

        assert!(reviews.publish(&second, "second command", now, deadline));
        let mut sibling = first.clone();
        sibling.key = second.key();
        assert!(!reviews.authorize(&sibling, now));
        assert!(reviews.settle(&sibling, Phase::AwaitingReview).is_none());
        assert!(reviews.authorize(&second, now));
        assert_eq!(
            reviews.next_authorized(now, |key| key == second.key()),
            Some(second.clone())
        );
        assert_eq!(reviews.claim(&first, now).unwrap().proposal, "probe");
        assert!(reviews.settle(&first, Phase::Executing).is_some());
        assert_eq!(
            reviews.by_ticket(&second).unwrap().phase(),
            Phase::Authorized
        );

        let next = reviews
            .begin(request, 0, 1, "first model round", now, deadline)
            .unwrap();
        assert_eq!(first.key(), next.key());
        assert_eq!(first.round(), next.round());
        assert_ne!(first, next);
        assert!(!reviews.publish(&first, "late probe", now, deadline));
        assert!(reviews.publish(&next, "first command", now, deadline));
        let mut wrong_round = next.clone();
        wrong_round.round += 1;
        assert!(!reviews.authorize(&wrong_round, now));
        assert!(reviews.authorize(&next, now));
        assert_eq!(reviews.claim(&next, now).unwrap().proposal, "first command");
        assert_eq!(
            reviews.claim(&second, now).unwrap().proposal,
            "second command"
        );
    }

    #[test]
    fn scheduled_target_review_expiry_and_cancel_settle_only_matching_lanes() {
        let (mut scheduler, job) = scheduler();
        let request = firing(&mut scheduler, &job);
        let now = Instant::now();
        let deadline = now + WINDOW;
        let mut reviews = Reviews::default();
        let expired = reviews
            .begin(Arc::clone(&request), 0, 1, (), now, deadline)
            .unwrap();
        let executing = reviews
            .begin_preapproved_probe(Arc::clone(&request), 1, (), "probe", now, deadline + WINDOW)
            .unwrap();
        let waiting = reviews
            .begin(Arc::clone(&request), 2, 2, (), now, deadline + WINDOW)
            .unwrap();
        assert_eq!(reviews.generating_count(), 2);
        assert!(reviews.claim(&executing, now).is_some());
        let ended = reviews.settle_inactive(deadline);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].ticket, expired);
        assert_eq!(ended[0].reason, EndReason::Expired);
        assert_eq!(reviews.for_run(request.id).count(), 2);
        assert_eq!(reviews.generating_count(), 1);

        scheduler.cancel(request.id).unwrap();
        assert!(reviews.get(&executing, deadline).is_none());
        assert_eq!(
            reviews.by_ticket(&executing).unwrap().phase(),
            Phase::Executing
        );
        let ended = reviews.settle_inactive(deadline);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].ticket, waiting);
        assert_eq!(ended[0].reason, EndReason::Cancelled);
        assert!(reviews.settle_inactive(deadline).is_empty());
        assert!(reviews.settle(&executing, Phase::Executing).is_some());
        assert_eq!(reviews.for_run(request.id).count(), 0);
    }
}
