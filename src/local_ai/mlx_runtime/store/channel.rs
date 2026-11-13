//! Apply signed MLX trust and select an update against the exact activation.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::local_ai::runtime_package::{Authorization, trust_changed, with_mlx_maintenance_io};
use crate::private_files::{Directory, EntryIdentity, FileStamp};

use super::{
    Index, InstallOptions, OPERATION, OsVersion, Package, Receipt, StagedArchive, State,
    check_cancelled, install_verified, invalid, lock, root,
};

pub(in super::super) struct Snapshot {
    directory: EntryIdentity,
    activation: Option<FileStamp>,
}

impl Snapshot {
    pub(in super::super) fn check(
        &self,
        directory: &Directory,
        current: Option<&FileStamp>,
    ) -> io::Result<()> {
        if directory.identity()? != self.directory || current != self.activation.as_ref() {
            return Err(invalid(
                "The installed MLX runtime or selected version changed",
            ));
        }
        Ok(())
    }

    pub(in super::super) fn check_current(&self, profile: &Path) -> io::Result<()> {
        let directory = root(profile, false)?;
        let _operation = lock(&directory, OPERATION, true)?;
        let (_, activation) = State::read(&directory)?;
        self.check(&directory, activation.as_ref())
    }
}

pub(in super::super) fn activate(
    profile: &Path,
    receipt: &Receipt,
    package: &Package,
    snapshot: &Snapshot,
    archive: &StagedArchive,
    authorize: &Authorization,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    snapshot.check_current(profile)?;
    drop(authorize()?);
    with_mlx_maintenance_io(profile, || {
        let (input, stamp) = archive.input()?;
        let guard = |directory: &Directory, current: Option<&FileStamp>| {
            snapshot.check(directory, current)?;
            authorize()
        };
        install_verified(
            profile,
            receipt,
            package,
            input,
            &stamp,
            cancelled,
            InstallOptions {
                commit_guard: Some(&guard),
                ..Default::default()
            },
        )
    })
}

pub(in super::super) struct Check {
    pub(in super::super) snapshot: Snapshot,
    pub(in super::super) candidate: Option<Package>,
    pub(in super::super) revoked: bool,
    pub(in super::super) current: bool,
    pub(in super::super) pending: bool,
}

pub(in super::super) fn check(
    profile: &Path,
    index: &Index,
    host: OsVersion,
    authorize: &Authorization,
    cancelled: &AtomicBool,
) -> io::Result<Check> {
    check_cancelled(cancelled)?;
    drop(authorize()?);
    let directory = root(profile, true)?;
    let _operation = lock(&directory, OPERATION, false)?;
    let (mut state, mut activation) = State::read(&directory)?;
    let catalog_changed = state
        .accepted
        .as_ref()
        .is_none_or(|accepted| accepted.sha256 != index.sha256);
    let new_withdrawals = index.revoked.iter().any(|id| {
        state
            .accepted
            .as_ref()
            .is_none_or(|accepted| !accepted.revoked.contains(id))
    });
    let prior_pin = state.pinned_runtime_id.clone();
    state.accept(index)?;
    state.clear_unavailable_pin(index, host);
    let revoked = state
        .active
        .as_ref()
        .is_some_and(|entry| state.withdrawn(&entry.runtime_id, &entry.installation_id));
    let previous_revoked = state
        .previous
        .as_ref()
        .is_some_and(|entry| state.withdrawn(&entry.runtime_id, &entry.installation_id));
    let pending = state.pending && !revoked;
    // A healthy first launch owns the exact activation receipt. Defer catalog
    // refreshes until it finishes unless the catalog adds a withdrawal.
    let must_publish = new_withdrawals || revoked || previous_revoked || !pending;
    if must_publish && (catalog_changed || previous_revoked || state.pinned_runtime_id != prior_pin)
    {
        if previous_revoked {
            state.previous = None;
        }
        let _permission: File = authorize()?;
        state.publish(&directory, activation.as_ref(), cancelled)?;
        directory.sync()?;
        activation = State::read(&directory)?.1;
        trust_changed();
    }
    check_cancelled(cancelled)?;
    let current = state.active.is_some() && !revoked;
    let candidate = if pending || (current && state.pinned_runtime_id.is_some()) {
        None
    } else {
        state
            .selectable(index, host)
            .into_iter()
            .find(|package| {
                state
                    .pinned_runtime_id
                    .as_ref()
                    .is_none_or(|pin| pin == &package.runtime_id)
            })
            .filter(|package| {
                state
                    .active
                    .as_ref()
                    .filter(|_| !revoked)
                    .is_none_or(|active| {
                        active.installation_id != package.installation_id
                            && index
                                .packages
                                .iter()
                                .find(|prior| prior.installation_id == active.installation_id)
                                .is_none_or(|prior| package.version_key() > prior.version_key())
                    })
            })
            .cloned()
    };
    Ok(Check {
        snapshot: Snapshot {
            directory: directory.identity()?,
            activation,
        },
        candidate,
        revoked,
        current,
        pending,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ed25519_dalek::SigningKey;
    use serde_json::{Value, json};

    use super::super::super::tests::{archive, descriptor, write_catalog};
    use super::*;

    const FIRST: &str = "mlx-0.31.3-kortty1-macos-aarch64";
    const SECOND: &str = "mlx-0.32.2-kortty1-macos-aarch64";

    fn receipt(
        profile: &Path,
        day: u8,
        packages: &[Value],
        revoked: &[&str],
        key: &SigningKey,
    ) -> Receipt {
        let (index, signature) = write_catalog(
            profile,
            &format!("signed-{day}"),
            day,
            packages,
            revoked,
            key,
        );
        Receipt::from_bytes(
            std::fs::read(index).unwrap(),
            std::fs::read(signature).unwrap(),
            &key.verifying_key(),
        )
        .unwrap()
    }

    #[test]
    fn mlx_background_trust_blocks_withdrawn_leases_and_activation_waits_for_idle() {
        let fixture = crate::snippets::tests::Profile::new();
        let profile = fixture.0.canonicalize().unwrap();
        let marker = profile.join("authority-fixture");
        std::fs::write(&marker, []).unwrap();
        let authority_file = marker.clone();
        let authorize = move || File::open(&authority_file);
        let host = OsVersion::parse("15.1").unwrap();
        let key = SigningKey::from_bytes(&[81; 32]);
        let cancelled = AtomicBool::new(false);
        let first = archive(FIRST, true, 0x0100_000c, None);
        let second = archive(SECOND, true, 0x0100_000c, None);
        let mut initial_package = descriptor(FIRST, &first);
        initial_package["mlxLmVersion"] = json!("0.31.3");
        let replacement = descriptor(SECOND, &second);
        let initial = receipt(
            &profile,
            4,
            std::slice::from_ref(&initial_package),
            &[],
            &key,
        );
        let selection = check(&profile, &initial.index, host, &authorize, &cancelled).unwrap();
        let package = selection.candidate.as_ref().unwrap();
        let staged = super::super::stage_download(&profile, package, &cancelled, |file| {
            file.write_all(&first)
        })
        .unwrap();
        assert_eq!(
            activate(
                &profile,
                &initial,
                package,
                &selection.snapshot,
                &staged,
                &authorize,
                &cancelled
            )
            .unwrap(),
            FIRST
        );
        drop(staged);

        let signed = receipt(
            &profile,
            5,
            &[initial_package, replacement.clone()],
            &[],
            &key,
        );
        let mut resident =
            super::super::Runtime::open_on(&profile, host, &key.verifying_key(), &cancelled)
                .unwrap();
        let pending = check(&profile, &signed.index, host, &authorize, &cancelled).unwrap();
        assert!(pending.pending && pending.candidate.is_none());
        resident.ready().unwrap();
        let offered = check(&profile, &signed.index, host, &authorize, &cancelled).unwrap();
        assert_eq!(offered.candidate.as_ref().unwrap().installation_id, SECOND);
        resident.revalidate().unwrap();

        let withdrawn = receipt(&profile, 6, &[replacement], &[FIRST], &key);
        let replacement = check(&profile, &withdrawn.index, host, &authorize, &cancelled).unwrap();
        assert!(replacement.revoked && !replacement.current);
        assert!(resident.revalidate().is_err());
        assert!(super::super::active(&profile).unwrap().is_none());
        assert!(offered.snapshot.check_current(&profile).is_err());
        assert!(check(&profile, &signed.index, host, &authorize, &cancelled).is_err());

        let package = replacement.candidate.as_ref().unwrap();
        let staged = super::super::stage_download(&profile, package, &cancelled, |file| {
            file.write_all(&second)
        })
        .unwrap();
        let install = |authorize: &Authorization| {
            activate(
                &profile,
                &withdrawn,
                package,
                &replacement.snapshot,
                &staged,
                authorize,
                &cancelled,
            )
        };
        assert_eq!(
            install(&authorize).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(resident);
        let calls = AtomicUsize::new(0);
        let revoked_permission = move || {
            if calls.fetch_add(1, Ordering::AcqRel) == 0 {
                File::open(&marker)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "runtime policy changed",
                ))
            }
        };
        assert_eq!(
            install(&revoked_permission).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(super::super::active(&profile).unwrap().is_none());
        assert_eq!(install(&authorize).unwrap(), SECOND);
        assert!(replacement.snapshot.check_current(&profile).is_err());
    }

    #[test]
    fn mlx_background_follows_an_available_pin_and_clears_a_withdrawn_pin() {
        let fixture = crate::snippets::tests::Profile::new();
        let profile = fixture.0.canonicalize().unwrap();
        let marker = profile.join("authority-fixture");
        std::fs::write(&marker, []).unwrap();
        let authorize = move || File::open(&marker);
        let host = OsVersion::parse("15.1").unwrap();
        let cancelled = AtomicBool::new(false);
        let key = SigningKey::from_bytes(&[82; 32]);
        let mut older = descriptor(FIRST, b"older signed archive");
        older["mlxLmVersion"] = json!("0.31.3");
        let newer = descriptor(SECOND, b"newer signed archive");
        let both = receipt(&profile, 9, &[older, newer.clone()], &[], &key);
        let newest = check(&profile, &both.index, host, &authorize, &cancelled).unwrap();
        assert_eq!(newest.candidate.unwrap().installation_id, SECOND);
        let directory = root(&profile, false).unwrap();
        let (mut state, prior) = State::read(&directory).unwrap();
        state.pinned_runtime_id = Some(FIRST.trim_end_matches("-macos-aarch64").into());
        state
            .publish(&directory, prior.as_ref(), &cancelled)
            .unwrap();
        let pinned = check(&profile, &both.index, host, &authorize, &cancelled).unwrap();
        assert_eq!(pinned.candidate.unwrap().installation_id, FIRST);
        assert!(newest.snapshot.check_current(&profile).is_err());

        let update = receipt(&profile, 10, &[newer], &[FIRST], &key);
        let stable = check(&profile, &update.index, host, &authorize, &cancelled).unwrap();
        assert_eq!(stable.candidate.unwrap().installation_id, SECOND);
        assert!(
            State::read(&directory)
                .unwrap()
                .0
                .pinned_runtime_id
                .is_none()
        );
    }
}
