use std::sync::Arc;

use crate::workspace::terminal_ai::web::{Evidence, system_with_guidance};

use super::Prompt;

#[derive(Clone)]
pub(super) struct ReviewedWeb {
    evidence: Arc<Evidence>,
    standing: bool,
    fresh: bool,
    run_evidence: Option<Arc<Evidence>>,
    before_round: Option<Arc<Evidence>>,
    system: String,
    input: String,
}

impl Prompt {
    pub(crate) fn with_web(
        mut self,
        evidence: Option<Arc<Evidence>>,
        standing: bool,
    ) -> Result<Self, String> {
        self.web = evidence.map(|evidence| ReviewedWeb {
            evidence,
            standing,
            fresh: false,
            run_evidence: None,
            before_round: None,
            system: String::new(),
            input: String::new(),
        });
        self.refresh_web()?;
        if standing {
            self.firing_input_preview()?;
        } else {
            self.check_one_use_context()?;
        }
        Ok(self)
    }

    pub(crate) fn with_fresh_web(mut self, evidence: Arc<Evidence>) -> Result<Self, String> {
        if self.web.is_some() {
            return Err("The scheduled request already has reviewed web excerpts.".into());
        }
        self = self.with_web(Some(evidence), true)?;
        if let Some(web) = &mut self.web {
            web.fresh = true;
        }
        Ok(self)
    }

    pub(crate) fn fresh_web(&self) -> Option<&Evidence> {
        self.web.as_ref().filter(|web| web.fresh).map(|web| {
            web.run_evidence
                .as_deref()
                .or(web.before_round.as_deref())
                .unwrap_or(web.evidence.as_ref())
        })
    }

    pub(crate) fn with_environment_web(mut self, evidence: Arc<Evidence>) -> Result<Self, String> {
        if !self.is_swarm_iterative() && !self.is_advisory() {
            return Err(
                "Environment web evidence requires a scheduled Swarm model request.".into(),
            );
        }
        let web = self
            .web
            .as_mut()
            .filter(|web| web.fresh && web.run_evidence.is_none() && web.before_round.is_none())
            .ok_or("The initial scheduled web excerpts are unavailable or already combined.")?;
        let combined = Arc::new(web.evidence.merged(&evidence)?);
        web.run_evidence = Some(std::mem::replace(&mut web.evidence, combined));
        self.refresh_web()?;
        self.firing_input_preview()?;
        Ok(self)
    }

    pub(crate) fn environment_web(&self) -> Option<&Evidence> {
        self.web
            .as_ref()
            .filter(|web| web.run_evidence.is_some())
            .map(|web| web.before_round.as_deref().unwrap_or(web.evidence.as_ref()))
    }

    pub(crate) fn with_round_web(mut self, evidence: &Evidence) -> Result<Self, String> {
        if !self.is_swarm_iterative() {
            return Err("Round web evidence requires an iterative Swarm request.".into());
        }
        let web = self
            .web
            .as_mut()
            .filter(|web| web.fresh && web.before_round.is_none())
            .ok_or("The scheduled web excerpts are unavailable or already combined.")?;
        let combined = Arc::new(web.evidence.merged(evidence)?);
        web.before_round = Some(std::mem::replace(&mut web.evidence, combined));
        self.refresh_web()?;
        Ok(self)
    }

    pub(crate) fn round_web(&self) -> Option<&Evidence> {
        self.web
            .as_ref()
            .filter(|web| web.before_round.is_some())
            .map(|web| web.evidence.as_ref())
    }

    pub(crate) fn base_for_standing_review(&self) -> std::borrow::Cow<'_, Self> {
        if self.fresh_web().is_some() {
            let mut base = self.clone();
            base.web = None;
            std::borrow::Cow::Owned(base)
        } else {
            std::borrow::Cow::Borrowed(self)
        }
    }

    pub(crate) fn web(&self) -> Option<&Evidence> {
        self.web.as_ref().map(|web| web.evidence.as_ref())
    }

    pub(super) fn reviewed_system(&self, standing: bool) -> Option<&str> {
        self.web
            .as_ref()
            .filter(|web| web.standing == standing)
            .map(|web| web.system.as_str())
    }

    pub(super) fn reviewed_input(&self, standing: bool) -> &str {
        self.web
            .as_ref()
            .filter(|web| web.standing == standing)
            .map_or(&self.input, |web| &web.input)
    }

    pub(super) fn advisory_input_with_web(&self, input: String) -> Result<String, String> {
        if let Some(web) = self.web.as_ref().filter(|web| web.standing) {
            encode_with_evidence(&input, &web.evidence)
        } else {
            Ok(input)
        }
    }

    pub(super) fn refresh_web(&mut self) -> Result<(), String> {
        let Some(web) = &self.web else {
            return Ok(());
        };
        web.evidence
            .require_mask(|text| self.knowledge_mask(text))?;
        let base = if web.standing {
            self.firing_system_with_knowledge()
        } else {
            self.base_proposal_system()
        };
        let system = system_with_guidance(base, self.transport.structured_system_suffix());
        if self.knowledge_mask(&system)? != system {
            return Err(
                "The model request with web excerpts no longer matches the reviewed mask.".into(),
            );
        }
        let input = encode_with_evidence(&self.input, &web.evidence)?;
        let web = self.web.as_mut().expect("reviewed web excerpts");
        web.system = system;
        web.input = input;
        Ok(())
    }
}

fn encode_with_evidence(input: &str, evidence: &Evidence) -> Result<String, String> {
    let failure = || "Could not prepare the model request with web excerpts.".to_owned();
    let mut input: serde_json::Value = serde_json::from_str(input).map_err(|_| failure())?;
    if !input.is_object() {
        return Err(failure());
    }
    evidence.append_to_json(&mut input);
    serde_json::to_string(&input).map_err(|_| failure())
}
