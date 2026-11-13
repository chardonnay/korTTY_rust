//! Optional local rsync client with an authenticated native SSH protocol bridge.

#[cfg(not(unix))]
use std::{io, path::Path};

#[cfg(unix)]
mod native;
#[cfg(unix)]
pub(super) use native::run;
#[cfg(unix)]
pub(crate) use native::{
    BRIDGE_ARGUMENT, program_digest, serve_bridge, upload_digest, upload_tree_inventory,
};

#[cfg(not(unix))]
pub(crate) fn program_digest(_: &Path) -> io::Result<[u8; 32]> {
    Err(io::Error::other(
        "Scheduled rsync transfers require a Unix rsync executable.",
    ))
}

#[cfg(not(unix))]
pub(crate) fn upload_digest(_: &Path) -> io::Result<(u64, [u8; 32])> {
    Err(io::Error::other(
        "Scheduled rsync uploads require a Unix rsync executable.",
    ))
}

#[cfg(not(unix))]
pub(crate) fn upload_tree_inventory(
    _: &Path,
) -> io::Result<kortty_server_archive::rsync_tree::Inventory> {
    Err(io::Error::other(
        "Scheduled rsync uploads require a Unix rsync executable.",
    ))
}

#[cfg(not(unix))]
pub(super) fn run(
    _: &kortty_jobs::RunRequest<crate::scheduler_window::SshCommand>,
    _: &crate::scheduler_window::rsync::Transfer,
    _: bool,
    _: &super::ExecutionPolicy,
    _: kortty_ssh::SshTarget,
    _: Vec<kortty_ssh::SshTarget>,
    _: &kortty_ssh::TrustStore,
) -> Result<super::Completion, super::Failure> {
    Err(super::Failure::blocked(
        "Scheduled rsync transfers require a Unix rsync executable.",
    ))
}
