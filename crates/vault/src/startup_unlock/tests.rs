use std::path::PathBuf;

use super::*;

const MASTER: &[u8] = b"fixture master password only";
const ROTATED: &[u8] = b"fixture rotated password only";

struct Directory(PathBuf);

impl Directory {
    fn create() -> Self {
        let suffix = hex(crate::random::<16>().unwrap());
        let path = std::env::temp_dir().join(format!("kortty-startup-unlock-{suffix}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        }
        #[cfg(windows)]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn remembered_key_authenticates_the_snapshot_and_cannot_unlock_after_rotation() {
    let directory = Directory::create();
    let path = directory.0.join("vault.bin");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    assert_eq!(
        Vault::startup_unlock_state(&path).unwrap(),
        StartupUnlockState::Disabled
    );
    assert!(matches!(
        Vault::forget_locked_startup_unlock(&path),
        Err(Error::Busy)
    ));
    let id = vault
        .save(
            crate::CredentialInput {
                label: "Startup fixture".into(),
                ..crate::CredentialInput::default()
            },
            crate::Secret::new("fixture-only credential".into()),
        )
        .unwrap();
    let mut saved = Record::default();
    vault
        .remember_with(|_, bytes| {
            saved.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        Vault::startup_unlock_state(&path).unwrap(),
        StartupUnlockState::Enabled
    );
    assert_eq!(
        storage::read_bounded(&storage::sidecar(&path, SUFFIX).unwrap(), ENABLED.len()).unwrap(),
        ENABLED
    );
    assert!(matches!(
        unlock(&path, |_| unreachable!()),
        Err(Error::Busy)
    ));
    drop(vault);

    let mut modified = saved.clone();
    modified[KEY_OFFSET] ^= 1;
    assert!(matches!(
        unlock(&path, |_| Ok(modified)),
        Err(Error::Authentication)
    ));
    let mut vault = unlock(&path, |_| Ok(saved.clone())).unwrap();
    assert_eq!(vault.secret(&id), Some("fixture-only credential"));
    vault.change_master_password(MASTER, ROTATED).unwrap();
    drop(vault);
    assert!(matches!(
        unlock(&path, |_| Ok(saved)),
        Err(Error::Authentication)
    ));

    let vault = Vault::unlock(&path, ROTATED).unwrap();
    let mut refreshed = Record::default();
    let expected_account = account(&vault.path);
    vault
        .remember_with(|account, bytes| {
            assert_eq!(account, expected_account);
            refreshed.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    drop(vault);
    assert_eq!(
        unlock(&path, |_| Ok(refreshed)).unwrap().secret(&id),
        Some("fixture-only credential")
    );
}

#[test]
fn unavailable_store_and_failed_removal_leave_automatic_unlock_disabled() {
    let directory = Directory::create();
    let path = directory.0.join("vault.bin");
    let vault = Vault::create(&path, MASTER).unwrap();
    assert!(matches!(
        vault.remember_with(|_, _| Err(Error::StartupCredentialUnavailable)),
        Err(Error::StartupCredentialUnavailable)
    ));
    assert_eq!(
        Vault::startup_unlock_state(&path).unwrap(),
        StartupUnlockState::RemovalPending
    );
    vault.remember_with(|_, _| Ok(())).unwrap();
    assert!(matches!(
        forget(&vault.path, |_| Err(Error::StartupCredentialUnavailable)),
        Err(Error::StartupCredentialUnavailable)
    ));
    let canonical = vault.path.clone();
    drop(vault);
    assert_eq!(
        Vault::startup_unlock_state(&path).unwrap(),
        StartupUnlockState::RemovalPending
    );
    assert!(matches!(
        unlock(&path, |_| unreachable!()),
        Err(Error::NotFound)
    ));
    forget(&canonical, |_| Ok(())).unwrap();
    assert_eq!(
        Vault::startup_unlock_state(&path).unwrap(),
        StartupUnlockState::Disabled
    );
}

#[test]
#[ignore = "requires a private test profile and an available operating-system credential store"]
fn native_credential_store_round_trip() {
    const REOPEN: &str = "KORTTY_TEST_STARTUP_UNLOCK_REOPEN";
    if let Some(path) = std::env::var_os(REOPEN) {
        drop(Vault::unlock_remembered(path).unwrap());
        return;
    }

    struct RemoveCredential(String);
    impl Drop for RemoveCredential {
        fn drop(&mut self) {
            let _ = native::delete(&self.0);
        }
    }

    let directory = Directory::create();
    let path = directory.0.join("vault.bin");
    let vault = Vault::create(&path, MASTER).unwrap();
    let cleanup = RemoveCredential(account(&vault.path));
    vault.remember_startup_unlock().unwrap();
    drop(vault);

    let mut vault = Vault::unlock_remembered(&path).unwrap();
    vault.change_master_password(MASTER, ROTATED).unwrap();
    vault.refresh_startup_unlock().unwrap();
    drop(vault);
    let reopened = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "startup_unlock::tests::native_credential_store_round_trip",
            "--ignored",
        ])
        .env(REOPEN, &path)
        .status()
        .unwrap();
    assert!(reopened.success());
    Vault::forget_locked_startup_unlock(&path).unwrap();
    assert_eq!(
        Vault::startup_unlock_state(&path).unwrap(),
        StartupUnlockState::Disabled
    );
    assert!(matches!(
        native::read(&cleanup.0),
        Err(Error::StartupCredentialUnavailable)
    ));
    assert!(matches!(
        Vault::unlock_remembered(&path),
        Err(Error::NotFound)
    ));
    drop(Vault::unlock(&path, ROTATED).unwrap());
}
