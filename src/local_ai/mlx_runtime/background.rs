//! Reviewed background use of the signed stable MLX channel.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ureq::Agent;

use crate::local_ai::runtime_package::{
    Authorization, Pending,
    online::{
        Activation, VerifiedAsset, download_verified_asset, fetch_release_metadata, policy::Mode,
    },
    work, work_with_agent,
};

use super::{Package, Receipt, host_version, online::fetch_catalog, release_key, store};

#[derive(Clone, Debug)]
pub(crate) enum Checked {
    Current { pending_first_launch: bool },
    NoCompatible { revoked: bool },
    Offer { id: String, revoked: bool },
    Stage(Arc<Selection>),
}

pub(crate) struct Selection {
    profile: PathBuf,
    receipt: Receipt,
    package: Package,
    snapshot: store::channel::Snapshot,
    authorize: Arc<Authorization>,
    pub(crate) revoked: bool,
}

impl fmt::Debug for Selection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MLX runtime selection")
            .field("package", &self.package.installation_id)
            .field("revoked", &self.revoked)
            .finish_non_exhaustive()
    }
}

pub(crate) struct Staged {
    selection: Arc<Selection>,
    archive: store::StagedArchive,
}

impl fmt::Debug for Staged {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Staged MLX runtime")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

pub(crate) fn check_policy(
    profile: PathBuf,
    mode: Mode,
    authorize: Arc<Authorization>,
) -> Result<Pending<Checked>, String> {
    if !mode.checks_channel() {
        return Err("Background MLX runtime checks are disabled.".into());
    }
    let host = host_version()?;
    let agent = Agent::new_with_config(crate::local_ai::connection_config(Duration::from_secs(30)));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        let receipt = fetch_catalog(&release_key(), |url, limit, allow_missing| {
            fetch_release_metadata(
                &agent,
                url,
                limit,
                allow_missing,
                Some(&*authorize),
                cancelled,
            )
        })?;
        let checked = store::channel::check(&profile, &receipt.index, host, &*authorize, cancelled)
            .map_err(|error| format!("Could not check the signed MLX runtime ({error})."))?;
        Ok(match checked.candidate {
            Some(package) if mode == Mode::AutomaticStable => Checked::Stage(Arc::new(Selection {
                profile,
                receipt,
                package,
                snapshot: checked.snapshot,
                authorize,
                revoked: checked.revoked,
            })),
            Some(package) => Checked::Offer {
                id: package.installation_id,
                revoked: checked.revoked,
            },
            None if checked.current => Checked::Current {
                pending_first_launch: checked.pending,
            },
            None => Checked::NoCompatible {
                revoked: checked.revoked,
            },
        })
    })
}

pub(crate) fn stage(selection: Arc<Selection>) -> Result<Pending<Arc<Staged>>, String> {
    let agent = Agent::new_with_config(crate::local_ai::connection_config(Duration::from_secs(
        2 * 60 * 60,
    )));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        selection
            .snapshot
            .check_current(&selection.profile)
            .map_err(|error| error.to_string())?;
        drop((selection.authorize)().map_err(|error| error.to_string())?);
        let asset = VerifiedAsset {
            url: &selection.package.download_url,
            size: selection.package.size_bytes,
            sha256: &selection.package.sha256,
        };
        let archive =
            store::stage_download(&selection.profile, &selection.package, cancelled, |file| {
                download_verified_asset(&agent, asset, file, Some(&*selection.authorize), cancelled)
            })
            .map_err(|error| format!("Could not stage the signed MLX runtime ({error})."))?;
        Ok(Arc::new(Staged { selection, archive }))
    })
}

pub(crate) fn apply(staged: Arc<Staged>) -> Result<Pending<Activation>, String> {
    work(move |cancelled| {
        let selection = &staged.selection;
        let installed = store::channel::activate(
            &selection.profile,
            &selection.receipt,
            &selection.package,
            &selection.snapshot,
            &staged.archive,
            &*selection.authorize,
            cancelled,
        );
        match installed {
            Ok(id) => Ok(Activation::Installed(id)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Activation::Waiting),
            Err(error) => Err(format!(
                "Could not activate the signed MLX runtime ({error})."
            )),
        }
    })
}
