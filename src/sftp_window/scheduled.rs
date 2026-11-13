//! Read-only proofs and bounded execution of reviewed folder-copy plans.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use kortty_ssh::sftp::Sftp;

use super::compare::folders::{self, Request};
use super::worker::Progress;
use super::worker::sync::{self, CopyPlan};
use super::{Location, Side};

#[cfg(unix)]
pub(crate) use super::worker::server_copy::Folder as CheckedRemoteFolder;
pub(crate) use super::worker::sync::{
    MAX_STANDING_ACTIONS, MAX_STANDING_BYTES, ReplacementPreview, StandingLimits,
};

pub(crate) type Client = Sftp<Box<dyn Read + Send>, Box<dyn Write + Send>>;

pub(crate) fn validate_excludes(patterns: &str) -> io::Result<()> {
    folders::validate_excludes(patterns)
}

pub(crate) struct Preview {
    pub(crate) proof: [u8; 32],
    pub(crate) roots: [String; 2],
    pub(crate) paths: Vec<(bool, String)>,
    pub(crate) replacements: Vec<ReplacementPreview>,
    pub(crate) bytes: u64,
}

impl Preview {
    fn from_plan(plan: &CopyPlan, proof: [u8; 32]) -> Self {
        Self {
            proof,
            roots: plan.roots().clone(),
            paths: plan
                .paths()
                .map(|(folder, path)| (folder, path.to_owned()))
                .collect(),
            replacements: plan.replacement_preview(),
            bytes: plan.bytes(),
        }
    }
}

pub(crate) struct StandingPreview {
    pub(crate) root_binding: [u8; 32],
    pub(crate) current: Preview,
}

pub(crate) struct Comparison<'a> {
    pub(crate) local: PathBuf,
    pub(crate) remote: Vec<u8>,
    pub(crate) excludes: &'a str,
    pub(crate) replace_changed: bool,
    pub(crate) to: Side,
}

impl Comparison<'_> {
    fn plan(
        self,
        client: &mut Client,
        cancelled: &impl Fn() -> bool,
        standing: Option<StandingLimits>,
    ) -> Result<CopyPlan, String> {
        if let Some(limits) = standing {
            if self.replace_changed {
                return Err("Standing approvals only create missing files and folders.".into());
            }
            if !limits.valid() {
                return Err("The standing copy limits are invalid.".into());
            }
            if self.local.file_name().is_none() || self.remote.as_slice() == b"/" {
                return Err("Standing copies require folders below the filesystem roots.".into());
            }
        }
        let request = Request::new(
            [Location::Local(self.local), Location::Remote(self.remote)],
            self.excludes,
        )
        .map_err(|error| error.to_string())?;
        let review = folders::read(client, request, cancelled).map_err(|error| error.message)?;
        let review = Arc::new(review);
        match standing {
            Some(limits) => CopyPlan::new_standing(review, self.to, limits),
            None if self.replace_changed => CopyPlan::new_replacing(review, self.to),
            None => CopyPlan::new(review, self.to),
        }
        .map_err(|error| error.to_string())
    }

    pub(crate) fn preview(
        self,
        client: &mut Client,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Preview, String> {
        let mut plan = self.plan(client, cancelled, None)?;
        let proof = plan
            .prove(client, cancelled)
            .map_err(|error| error.message)?;
        Ok(Preview::from_plan(&plan, proof))
    }

    pub(crate) fn preview_standing(
        self,
        client: &mut Client,
        limits: StandingLimits,
        cancelled: &impl Fn() -> bool,
    ) -> Result<StandingPreview, String> {
        let mut plan = self.plan(client, cancelled, Some(limits))?;
        let root_binding = plan
            .standing_binding()
            .ok_or("The standing copy folders could not be verified")?;
        let proof = plan
            .prove(client, cancelled)
            .map_err(|error| error.message)?;
        Ok(StandingPreview {
            root_binding,
            current: Preview::from_plan(&plan, proof),
        })
    }

    pub(crate) fn apply(
        self,
        client: &mut Client,
        expected: [u8; 32],
        cancelled: &impl Fn() -> bool,
        disconnected: &impl Fn() -> bool,
        claim: impl FnOnce() -> io::Result<()>,
    ) -> Result<Applied, String> {
        self.apply_authorized(client, expected, cancelled, disconnected, claim, &|| Ok(()))
    }

    pub(crate) fn apply_authorized(
        self,
        client: &mut Client,
        expected: [u8; 32],
        cancelled: &impl Fn() -> bool,
        disconnected: &impl Fn() -> bool,
        claim: impl FnOnce() -> io::Result<()>,
        authorize: &impl Fn() -> io::Result<()>,
    ) -> Result<Applied, String> {
        let mut plan = self.plan(client, cancelled, None)?;
        let proof = plan
            .prove(client, cancelled)
            .map_err(|error| error.message)?;
        if proof != expected {
            return Err(
                "The reviewed folders, actions, or file content changed. Compare again.".into(),
            );
        }
        let progress = Progress::new(Some(plan.bytes()));
        Ok(sync::apply_scheduled(
            client,
            &plan,
            &progress,
            cancelled,
            disconnected,
            claim,
            authorize,
        )
        .into())
    }

    pub(crate) fn apply_standing(
        self,
        client: &mut Client,
        expected_roots: [u8; 32],
        limits: StandingLimits,
        cancelled: &impl Fn() -> bool,
        disconnected: &impl Fn() -> bool,
        authorize: impl Fn(usize, u64) -> io::Result<()>,
    ) -> Result<Applied, String> {
        let mut plan = self.plan(client, cancelled, Some(limits))?;
        if plan.standing_binding() != Some(expected_roots) {
            return Err("The standing copy folders changed. Review the job again.".into());
        }
        // Keep fresh source hashes on the exact in-memory plan for this firing.
        plan.prove(client, cancelled)
            .map_err(|error| error.message)?;
        let actions = plan.paths().count();
        let progress = Progress::new(Some(plan.bytes()));
        Ok(
            sync::apply_standing(client, &plan, &progress, cancelled, disconnected, || {
                authorize(actions, plan.bytes())
            })
            .into(),
        )
    }
}

pub(crate) struct Applied {
    pub(crate) copied: usize,
    pub(crate) created: usize,
    pub(crate) replaced: usize,
    pub(crate) recovery_prefix: Option<String>,
    pub(crate) error: Option<String>,
}

impl From<sync::Applied> for Applied {
    fn from(applied: sync::Applied) -> Self {
        Self {
            copied: applied.copied,
            created: applied.created,
            replaced: applied.replaced,
            recovery_prefix: applied.recovery_prefix,
            error: applied.result.err().map(|error| error.message),
        }
    }
}
