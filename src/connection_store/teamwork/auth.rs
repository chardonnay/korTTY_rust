//! Local authentication preferences for shared connections.

use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::ssh_dialog::valid_username;

use super::super::validate_credential_id;
use super::invalid;

const FILE: &str = "teamwork-auth.json";
const LOCK: &str = "teamwork-auth.lock";
const MAX_BYTES: u64 = 4096;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum DefaultAuth {
    #[default]
    Prompt,
    Password {
        credential: String,
    },
    Key {
        id: u64,
        username: String,
    },
    TemporaryKey {
        username: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum AuthSave {
    Saved,
    Changed(DefaultAuth),
}

impl DefaultAuth {
    pub(crate) fn validate(&self) -> io::Result<()> {
        match self {
            Self::Password { credential } => validate_credential_id(credential),
            Self::Key { id: 0, .. } => Err(invalid("Select a registered SSH key.")),
            Self::Key { username, .. } | Self::TemporaryKey { username }
                if !username.is_empty() && !valid_username(username) =>
            {
                Err(invalid("Invalid Teamwork SSH username."))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn load(root: &Path) -> io::Result<Self> {
        match Directory::open(root, false) {
            Ok(directory) => Self::read(&directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    fn read(directory: &Directory) -> io::Result<Self> {
        let file = match directory.open_private_file(FILE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let mut bytes = Zeroizing::new(Vec::new());
        (&file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        stamp.require_unchanged(&file)?;
        stamp.require_unchanged(&directory.open_private_file(FILE)?)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid(
                "Teamwork authentication settings exceed the size limit.",
            ));
        }
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("Invalid Teamwork authentication settings."))?;
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn save(&self, root: &Path, expected: &Self) -> io::Result<AuthSave> {
        self.validate()?;
        let directory = Directory::open(root, true)?;
        let lock = directory.open_lock(LOCK)?;
        require_single_link(&lock)?;
        lock.try_lock().map_err(io::Error::from)?;
        let current = Self::read(&directory)?;
        if current != *expected && current != *self {
            return Ok(AuthSave::Changed(current));
        }
        if current != *self {
            let bytes = serde_json::to_vec_pretty(self).map_err(invalid)?;
            directory.replace_file(FILE, &bytes)?;
        }
        Ok(AuthSave::Saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teamwork_auth_preferences_round_trip_and_reject_stale_or_invalid_edits() {
        let scratch = crate::snippets::tests::Profile::new();
        let root = &scratch.0.join("teamwork-auth-profile");
        let prompt = DefaultAuth::default();
        assert_eq!(DefaultAuth::load(root).unwrap(), prompt);
        let password = DefaultAuth::Password {
            credential: "credential-123".into(),
        };
        assert!(matches!(
            password.save(root, &prompt).unwrap(),
            AuthSave::Saved
        ));
        assert_eq!(DefaultAuth::load(root).unwrap(), password);
        assert!(matches!(
            password.save(root, &prompt).unwrap(),
            AuthSave::Saved
        ));

        let key = DefaultAuth::Key {
            id: 42,
            username: "ops".into(),
        };
        assert!(
            matches!(key.save(root, &prompt).unwrap(), AuthSave::Changed(current) if current == password)
        );
        let bad = DefaultAuth::TemporaryKey {
            username: "ops\nother".into(),
        };
        assert!(bad.save(root, &password).is_err());
        assert_eq!(DefaultAuth::load(root).unwrap(), password);
        assert!(matches!(
            key.save(root, &password).unwrap(),
            AuthSave::Saved
        ));
        assert_eq!(DefaultAuth::load(root).unwrap(), key);
        let temporary = DefaultAuth::TemporaryKey {
            username: String::new(),
        };
        assert!(matches!(
            temporary.save(root, &key).unwrap(),
            AuthSave::Saved
        ));
        assert_eq!(DefaultAuth::load(root).unwrap(), temporary);

        let corrupt = br#"{"method":"temporary-key","username":"ops","key":"unused"}"#;
        let directory = Directory::open(root, false).unwrap();
        directory.replace_file(FILE, corrupt).unwrap();
        assert!(DefaultAuth::load(root).is_err());
        assert!(prompt.save(root, &temporary).is_err());
        assert_eq!(std::fs::read(root.join(FILE)).unwrap(), corrupt);
    }
}
