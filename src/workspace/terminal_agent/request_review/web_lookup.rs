//! Reviews public queries and excerpts for a single interactive model request.

use super::*;

const CHANGED: &str = "The web search settings or credentials changed. Review the search again.";

pub(super) struct Lookup {
    scope: web::Scope,
    state: web::State,
    ticket: u64,
    authority: Option<WebAuthority>,
    evidence: Option<web::Evidence>,
}

#[derive(Clone)]
pub(super) struct WebAuthority {
    scope: web::Scope,
    vault: Weak<()>,
    transport: local_ai::Profile,
    credential: Option<Zeroizing<String>>,
    provider_credentials: Vec<(String, Option<Zeroizing<String>>)>,
}

impl WebAuthority {
    pub(super) fn capture(
        scope: &web::Scope,
        workspace: &Workspace,
        authority: &Authority,
    ) -> Self {
        let search = scope.search_credential_env();
        let provider_credentials = search
            .into_iter()
            .chain(
                scope
                    .page_credential_env()
                    .filter(|name| Some(*name) != search),
            )
            .map(|name| (name.to_owned(), local_ai::credential_value(name).ok()))
            .collect();
        Self {
            scope: scope.clone(),
            vault: workspace.vault_change_revision(),
            transport: authority.transport.0.clone(),
            credential: authority.knowledge_credential(),
            provider_credentials,
        }
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if self.vault.strong_count() == 0
            || self.credential != self.transport.completion_secret()
            || self
                .provider_credentials
                .iter()
                .any(|(name, reviewed)| &local_ai::credential_value(name).ok() != reviewed)
        {
            return Err(CHANGED.into());
        }
        self.scope.require_current()
    }

    pub(super) fn mask(
        &self,
        run: &Run,
        vault: Option<&VaultWindow>,
    ) -> Result<knowledge_review::Mask, String> {
        let secrets = Secrets::default();
        for (_, credential) in &self.provider_credentials {
            if let Some(credential) = credential {
                secrets.add(credential);
                let encoded = Zeroizing::new(
                    url::form_urlencoded::byte_serialize(credential.as_bytes()).collect::<String>(),
                );
                secrets.add(&encoded);
            }
        }
        let overlay = secrets.seal().map_err(|error| error.to_string())?;
        let redactor = run
            .redactor
            .with_overlay(&overlay)
            .map_err(|error| error.to_string())?;
        Ok(knowledge_review::Mask::new(
            redactor,
            self.credential.clone(),
            vault,
        ))
    }

    pub(super) fn require_current(&self, workspace: &Workspace) -> Result<(), String> {
        if !workspace.clipboard.policy.ai_web_search_allowed
            || !self.vault.ptr_eq(&workspace.vault_change_revision())
        {
            return Err(CHANGED.into());
        }
        self.check()
    }
}

impl Lookup {
    pub(super) fn scope(&self) -> &web::Scope {
        &self.scope
    }
    pub(super) fn new(authority: &Authority, run: &Run, allowed: bool) -> Option<Self> {
        let scope = web::Scope::new(
            Some(&authority.config),
            authority.profile.id,
            allowed && !authority.private,
        )?
        .with_redactor(&run.redactor);
        Some(Self {
            scope,
            state: web::State::default(),
            ticket: 0,
            authority: None,
            evidence: None,
        })
    }

    fn clear(&mut self) {
        self.state = web::State::Closed;
        self.authority = None;
        self.evidence = None;
    }
}

impl Review {
    pub(in crate::workspace::terminal_agent) fn web_available(&self) -> bool {
        self.web_lookup.is_some()
    }

    pub(in crate::workspace::terminal_agent) fn web_active(&self) -> bool {
        self.web_lookup
            .as_ref()
            .is_some_and(|lookup| lookup.state.active())
    }

    pub(in crate::workspace::terminal_agent) fn web_included(&self) -> bool {
        self.web_lookup
            .as_ref()
            .is_some_and(|lookup| lookup.evidence.is_some())
    }

    pub(in crate::workspace::terminal_agent) fn web_view(
        &self,
        epoch: u64,
    ) -> Option<iced::Element<'_, Message>> {
        self.web_lookup
            .as_ref()?
            .state
            .view()
            .map(move |view| view.map(move |message| Message::Web(epoch, message)))
    }

    pub(super) fn reviewed_web_authority(&self) -> Option<WebAuthority> {
        let lookup = self.web_lookup.as_ref()?;
        lookup.evidence.as_ref()?;
        lookup.authority.clone()
    }

    pub(super) fn require_current_web(&self, workspace: &Workspace) -> Result<(), String> {
        if let Some(authority) = self.reviewed_web_authority() {
            authority.require_current(workspace)?;
        }
        Ok(())
    }

    pub(super) fn web_context(
        &self,
        run: &Run,
        vault: Option<&VaultWindow>,
    ) -> Result<Option<String>, String> {
        let additional = self
            .web_lookup
            .as_ref()
            .and_then(|lookup| lookup.evidence.as_ref());
        if run.swarm_web.is_none() && additional.is_none() {
            return Ok(None);
        }
        let masking = match self.reviewed_web_authority() {
            Some(authority) => authority.mask(run, vault)?,
            None => knowledge_review::Mask::new(
                run.redactor.clone(),
                self.authority.knowledge_credential(),
                vault,
            ),
        };
        if let Some(baseline) = &run.swarm_web {
            return baseline.prompt_context_with(&masking, additional).map(Some);
        }
        additional
            .map(|evidence| {
                evidence.require_mask(|value| masking.apply(value))?;
                let mut context = String::new();
                evidence.append_to(&mut context);
                Ok(context)
            })
            .transpose()
    }
}

impl Workspace {
    pub(super) fn review_web_action(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
        message: web::Message,
    ) -> Result<Task<Desktop>, String> {
        use web::Message::{Close, Finished, Include, Open, Send};

        let Some(lookup) = review.web_lookup.as_ref() else {
            return Ok(Task::none());
        };
        if matches!(message, Close) {
            return self.close_review_web(
                id,
                run,
                review,
                "Review the masked model request before sending.",
            );
        }
        if run.paused && !matches!(message, Finished(..)) {
            return Ok(Task::none());
        }
        if lookup.evidence.is_some() || (!lookup.state.active() && !matches!(message, Open)) {
            return Ok(Task::none());
        }
        self.require_review_action(run, review)?;
        let authority = lookup
            .authority
            .clone()
            .unwrap_or_else(|| WebAuthority::capture(&lookup.scope, self, &review.authority));
        if let Err(error) = authority.require_current(self) {
            run.notice = error.clone();
            return self.close_review_web(id, run, review, &error);
        }

        let guard = matches!(message, Send)
            .then(|| self.review_dispatch_guard(run, review))
            .transpose()?;
        let mask = authority.mask(run, self.vault.as_ref())?;
        let lookup = review.web_lookup.as_mut().expect("available web lookup");
        lookup.authority = Some(authority.clone());
        let ticket = matches!(message, Send).then(|| {
            lookup.ticket += 1;
            lookup.ticket
        });
        let include = matches!(message, Include) && matches!(lookup.state, web::State::Results(..));
        let epoch = run.epoch;
        let result = lookup.state.update(
            message,
            lookup.scope.clone(),
            move |value| mask.apply(value),
            ticket,
            move || {
                authority.check()?;
                let guard = guard.as_ref().ok_or(CHANGED)?;
                guard()
            },
            move |result| event(id, Message::Web(epoch, result)),
        );
        match result {
            Ok(_) if include => self.include_review_web(id, run, review),
            Ok(task) => Ok(task),
            Err(error) => {
                run.notice = error;
                Ok(Task::none())
            }
        }
    }

    fn include_review_web(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
    ) -> Result<Task<Desktop>, String> {
        let lookup = review.web_lookup.as_mut().expect("open web lookup");
        let Some((scope, excerpts)) = lookup.state.included() else {
            return Ok(Task::none());
        };
        let results = web::State::Results(scope.clone(), Arc::clone(excerpts));
        let outcome = match web::Evidence::new(scope, excerpts) {
            Ok(evidence) => {
                lookup.evidence = Some(evidence);
                review.set_guidance(run, review.guidance.clone(), self.vault.as_ref())
            }
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            let lookup = review.web_lookup.as_mut().expect("open web lookup");
            lookup.evidence = None;
            lookup.state = results;
            run.notice = error;
            return Ok(Task::none());
        }
        let epoch = run.next(
            Phase::ModelReview,
            "Review the model request with the masked web excerpts before sending.",
        );
        Ok(review.refresh_quota(id, epoch))
    }

    fn close_review_web(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
        notice: &str,
    ) -> Result<Task<Desktop>, String> {
        let Some(lookup) = review
            .web_lookup
            .as_mut()
            .filter(|lookup| lookup.authority.is_some())
        else {
            return Ok(Task::none());
        };
        let included = lookup.evidence.is_some();
        lookup.clear();
        if included {
            review.set_guidance(run, review.guidance.clone(), self.vault.as_ref())?;
        }
        let epoch = run.next(Phase::ModelReview, notice);
        Ok(review.refresh_quota(id, epoch))
    }
}
