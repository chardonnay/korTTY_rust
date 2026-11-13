//! A reviewed split request that waits for its exact analysis record.

use crate::snippet_analysis::model::modularization::Plan;

use super::*;

pub(in crate::workspace::snippet_access::analysis) struct Prepared {
    pub(in crate::workspace::snippet_access::analysis) request: PlanRequest,
    source: SavedSource,
}

struct SavedSource {
    library: Library,
    originals: Vec<(String, SnippetId)>,
    source_hash: String,
    rendered_hash: String,
}

pub(in crate::workspace::snippet_access::analysis) struct Approved {
    request: Option<PlanRequest>,
    source: SavedSource,
    config: Arc<Configuration>,
    guidance: SkillGuidance,
    transport: local_ai::Profile,
    profile: ProfileRef,
}

pub(in crate::workspace::snippet_access::analysis) struct Deferred {
    record: SnippetId,
    approved: Approved,
    claim: Option<Arc<apply::Claim>>,
    skill_usage: Option<SkillUsageReceipt>,
}

pub(in crate::workspace::snippet_access::analysis) struct OutboundFollowup {
    pub(in crate::workspace::snippet_access::analysis) id: SnippetId,
    pub(in crate::workspace::snippet_access::analysis) profile: ProfileChoice,
    pub(in crate::workspace::snippet_access::analysis) transport: local_ai::Profile,
    pub(in crate::workspace::snippet_access::analysis) source_hash: String,
    pub(in crate::workspace::snippet_access::analysis) rendered_hash: String,
    pub(in crate::workspace::snippet_access::analysis) config: Arc<Configuration>,
    pub(in crate::workspace::snippet_access::analysis) guidance: SkillGuidance,
    pub(in crate::workspace::snippet_access::analysis) instructions: String,
    pub(in crate::workspace::snippet_access::analysis) vault_revision: Weak<()>,
}

impl OutboundFollowup {
    pub(super) fn record<'a>(&self, page: &'a Page) -> Option<&'a Record> {
        page.history
            .as_ref()?
            .records
            .iter()
            .find(|record| record.id == self.id)
    }

    pub(super) fn require_current(
        &self,
        config: &Arc<Configuration>,
        profile: &ProfileChoice,
        transport: &local_ai::Profile,
        capture: &Capture,
        record: &Record,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(config, &self.config)
            || &self.transport != transport
            || !(profile.cli || profile.remote)
            || self.profile.reference != profile.reference
            || record.provenance.profile_id != profile.reference
            || record.modularization.is_some()
            || self.vault_revision.strong_count() == 0
            || capture.source_sha256 != self.source_hash
            || capture.rendered_sha256 != self.rendered_hash
        {
            return Err(
                "The saved analysis, source or AI profile changed. Open a new split review.".into(),
            );
        }
        Ok(())
    }
}

impl Prepared {
    pub(in crate::workspace::snippet_access::analysis) fn new(
        window: &SnippetWindow,
        page: &Page,
        snapshot: &Snapshot,
        source: &Source,
        rendered_hash: &str,
        (skills, profile): (&str, &local_ai::Profile),
    ) -> Result<Option<Self>, String> {
        let Ok(originals) = capture_originals(window, page, snapshot) else {
            return Ok(None);
        };
        let request = PlanRequest::new(
            snapshot,
            &source.report_language_code,
            &page.instructions,
            skills,
        )?
        .with_profile(profile);
        Ok(Some(Self {
            request,
            source: SavedSource {
                library: window.latest_library().clone(),
                originals,
                source_hash: source.sha256.clone(),
                rendered_hash: rendered_hash.into(),
            },
        }))
    }

    pub(in crate::workspace::snippet_access::analysis) fn matches_source(
        &self,
        window: &SnippetWindow,
        page: &Page,
    ) -> bool {
        self.source.matches(window, page, None)
    }

    pub(in crate::workspace::snippet_access::analysis) fn approve(
        self,
        config: Arc<Configuration>,
        transport: local_ai::Profile,
        profile: ProfileRef,
        guidance: SkillGuidance,
    ) -> Approved {
        Approved {
            request: Some(self.request),
            source: self.source,
            config,
            guidance,
            transport,
            profile,
        }
    }
}

impl SavedSource {
    fn matches(&self, window: &SnippetWindow, page: &Page, record: Option<&Record>) -> bool {
        self.library.same_revision(window.latest_library())
            && snapshot_for(window, page, record).is_ok_and(|snapshot| {
                snapshot
                    .render()
                    .and_then(|rendered| digest(&rendered))
                    .is_ok_and(|hash| hash == self.rendered_hash)
                    && capture_originals(window, page, &snapshot)
                        .is_ok_and(|originals| originals == self.originals)
            })
    }
}

impl Deferred {
    pub(in crate::workspace::snippet_access::analysis) fn new(
        record: SnippetId,
        approved: Approved,
    ) -> Self {
        Self {
            record,
            approved,
            claim: None,
            skill_usage: None,
        }
    }

    pub(in crate::workspace::snippet_access::analysis) fn record_usage(self) {
        if let Some(receipt) = self.skill_usage {
            receipt.record();
        }
    }

    fn revalidate(
        &self,
        window: &SnippetWindow,
        page: &Page,
        config: Option<&Arc<Configuration>>,
    ) -> Result<(), String> {
        let approved = &self.approved;
        let record = page.history.as_ref().and_then(|history| {
            history
                .records
                .iter()
                .find(|record| record.id == self.record)
        });
        if config.is_some_and(|config| {
            Arc::ptr_eq(config, &approved.config)
                && config
                    .resolve_profile_reference(&approved.profile)
                    .and_then(|id| config.code_analysis_transport(id).ok())
                    .is_some_and(|(transport, _)| transport == approved.transport)
        }) && record.is_some_and(|record| {
            record.purpose != Purpose::Verify
                && record.provenance.profile_id == approved.profile
                && record.modularization.is_none()
                && record.source.sha256 == approved.source.source_hash
                && approved.source.matches(window, page, Some(record))
        }) {
            Ok(())
        } else {
            Err("The saved source or AI profile changed. Review the split request again.".into())
        }
    }
}

pub(in crate::workspace::snippet_access::analysis) fn cancel(page: &mut Page) {
    page.outbound_followup = None;
    if let State::Modularization(pane) = &mut page.state
        && pane.is_outbound_followup()
    {
        if matches!(pane.as_ref(), Pane::Working(_)) {
            pane.abort();
        } else {
            page.state = State::Ready;
        }
    }
    if let State::Requesting(run) = &mut page.state {
        run.plan = None;
        run.outbound_plan = None;
    }
    if let State::Review(review) = &mut page.state {
        review.outbound_plan = None;
    }
    if matches!(&page.state, State::Review(review) if review.plan.is_some()) {
        page.state = State::Ready;
    }
    if let Some(deferred) = page.automatic_plan.take()
        && deferred.approved.request.is_none()
    {
        if let State::HistoryWork(_, _, Some(cancelled)) = &page.state {
            cancelled.store(true, Ordering::Release);
        }
        page.state = State::Ready;
        page.automatic_diagram = None;
    }
}

impl Workspace {
    pub(in crate::workspace::snippet_access::analysis) fn start_outbound_split_followup(
        &mut self,
        id: window::Id,
        subject: Subject,
    ) -> Option<Task<Message>> {
        if !self.window_settings.snippet_analysis_auto_modularization
            || self.exiting
            || self.scheduler_draining
            || self.hosted_close_locks(id)
        {
            return None;
        }
        let surface = self.snippets.get_mut(&id)?;
        if surface.analysis.active != Some(subject) {
            return None;
        }
        let page = surface.analysis.page_mut(subject)?;
        if !matches!(page.state, State::Ready) {
            return None;
        }
        let followup = page.outbound_followup.take()?;
        Some(self.prepare_modularization(id, Action::Plan, Some(followup)))
    }

    pub(in crate::workspace::snippet_access::analysis) fn start_approved_split(
        &mut self,
        id: window::Id,
        subject: Subject,
    ) -> Option<Task<Message>> {
        let enabled = self.window_settings.snippet_analysis_auto_modularization
            && !self.exiting
            && !self.scheduler_draining
            && !self.hosted_close_locks(id);
        let config = self.ai_configuration();
        let token = self.allocate();
        let surface = self.snippets.get_mut(&id)?;
        let page = surface.analysis.page_mut(subject)?;
        let mut deferred = page.automatic_plan.take()?;
        if !enabled || !matches!(page.state, State::Ready) {
            return None;
        }
        let started = (|| {
            deferred.revalidate(&surface.window, page, config.as_ref())?;
            let claim = apply::Claim::new(&surface.window, subject)?;
            let request = deferred
                .approved
                .request
                .take()
                .ok_or("The reviewed split request is no longer available.")?;
            let worker_claim = Arc::clone(&claim);
            let pending = request.start(deferred.approved.transport.clone(), move |plan| {
                let _claim = worker_claim;
                Ok(plan)
            })?;
            deferred.claim = Some(claim);
            Ok::<_, String>(pending)
        })();
        match started {
            Ok(local_ai::Pending { reply, cancel }) => {
                let approved = &deferred.approved;
                deferred.skill_usage = approved.config.skill_usage(
                    &approved.guidance,
                    &approved.transport,
                    SkillUsageSurface::Workflow,
                    false,
                );
                page.automatic_plan = Some(deferred);
                page.state = State::Modularization(Box::new(Pane::Working(Work {
                    token,
                    label: "Planning a split for the saved analysis…",
                    cancel: Some(cancel),
                    skill_usage: None,
                    outbound: None,
                })));
                Some(Task::perform(
                    async move {
                        reply
                            .recv()
                            .await
                            .unwrap_or_else(|_| Err("The split planner stopped.".into()))
                    },
                    move |result| event(id, Event::AutomaticPlanned(subject, token, result)),
                ))
            }
            Err(error) => {
                page.status = Some(error);
                None
            }
        }
    }

    pub(in crate::workspace::snippet_access::analysis) fn finish_approved_split(
        &mut self,
        id: window::Id,
        subject: Subject,
        token: u64,
        result: Result<Plan, String>,
    ) -> Task<Message> {
        let enabled = self.window_settings.snippet_analysis_auto_modularization
            && !self.exiting
            && !self.scheduler_draining
            && !self.hosted_close_locks(id);
        let config = self.ai_configuration();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .page_mut(subject)
            .filter(|page| is_work(page, token))
        else {
            return Task::none();
        };
        let Some(deferred) = page.automatic_plan.take() else {
            return Task::none();
        };
        page.state = State::Ready;
        let save = (|| {
            if !enabled {
                return Err("The reviewed split request was cancelled.".into());
            }
            deferred.revalidate(&surface.window, page, config.as_ref())?;
            let job = Job {
                id: SnippetId::new().map_err(|error| error.to_string())?,
                revision: 1,
                source_sha256: deferred.approved.source.source_hash.clone(),
                rendered_sha256: deferred.approved.source.rendered_hash.clone(),
                sources: deferred.approved.source.originals.clone(),
                plan: result?,
                outbound: None,
                generation: None,
                publication: None,
                published: false,
            };
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            Ok::<_, String>((store, job))
        })();
        let (store, job) = match save {
            Ok(save) => save,
            Err(error) => {
                page.status = Some(error);
                return self.start_saved_analysis_followups(id, subject);
            }
        };
        let pre = deferred.record;
        let source = deferred.approved.source.library.clone();
        let claim = deferred.claim.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        page.automatic_plan = Some(deferred);
        page.state = State::HistoryWork(
            token,
            "Saving the split plan…",
            Some(Arc::clone(&cancelled)),
        );
        history_task(
            id,
            subject,
            token,
            HistoryChange::AutomaticPlan,
            move || {
                let _claim = claim;
                store.save_modularization_while(subject, pre, None, Some(job), || {
                    !cancelled.load(Ordering::Acquire)
                        && Library::open(source.path().to_path_buf())
                            .is_ok_and(|current| current.same_revision(&source))
                })
            },
        )
    }
}
