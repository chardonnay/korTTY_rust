use super::{
    Auth, AuthMethod, AuthPreferences, SavedLogin, invalid, valid_text, validate_credential_id,
};
use std::io;

impl AuthPreferences {
    pub(super) fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        for id in self.password.iter().chain(self.passphrase.iter()) {
            validate_credential_id(id)?;
        }
        if !valid_text(&self.key_file, 4096, true) {
            return Err(invalid("invalid inactive SSH authentication settings"));
        }
        Ok(())
    }

    /// Active fields live in Auth. Inactive fields must never override them on reopen.
    pub(crate) fn omit_active(&mut self, auth: &Auth) {
        if auth.method() == AuthMethod::Password || auth.credential_reference().is_some() {
            self.password = None;
        }
        if matches!(auth, Auth::KeyFile { .. }) {
            self.key_file.clear();
            self.passphrase = None;
        }
    }
}

impl SavedLogin {
    /// The pre-existing execution identity, excluding inert editor preferences.
    pub(crate) fn execution_identity(&self) -> Self {
        let mut login = self.clone();
        login.preferences = AuthPreferences::default();
        login
    }

    pub(crate) fn same_execution_identity(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint
            && self.auth == other.auth
            && self.keyboard_interactive == other.keyboard_interactive
    }

    pub(crate) fn references_vault(&self) -> bool {
        self.auth.has_vault_reference()
            || self.preferences.password.is_some()
            || self.preferences.passphrase.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> SavedLogin {
        SavedLogin {
            endpoint: crate::ssh_dialog::LoginTarget {
                host: "host.example".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::Prompt,
            keyboard_interactive: false,
            preferences: Default::default(),
        }
    }

    #[test]
    fn legacy_login_encoding_is_unchanged_and_preferences_round_trip() {
        let mut login = login();
        let legacy = serde_json::to_vec(&login).unwrap();
        assert!(!String::from_utf8_lossy(&legacy).contains("preferences"));
        assert_eq!(
            serde_json::from_slice::<SavedLogin>(&legacy).unwrap(),
            login
        );
        login.preferences.key_file = "relative unfinished path".into();
        login.preferences.passphrase = Some("inactive-passphrase".into());
        login.validate().unwrap();
        let encoded = serde_json::to_vec(&login).unwrap();
        assert_eq!(
            serde_json::from_slice::<SavedLogin>(&encoded).unwrap(),
            login
        );
        assert_eq!(
            serde_json::to_vec(&login.execution_identity()).unwrap(),
            legacy
        );
        assert!(login.same_execution_identity(&serde_json::from_slice(&legacy).unwrap()));
        login.auth = Auth::KeyFile {
            path: "relative unfinished path".into(),
            passphrase: None,
        };
        assert!(login.validate().is_err());
    }

    #[test]
    fn inactive_references_are_bounded_and_count_as_backup_dependencies() {
        let mut login = login();
        login.preferences.passphrase = Some("passphrase-reference".into());
        login.validate().unwrap();
        assert!(login.references_vault());
        login.preferences.key_file = "bad\npath".into();
        assert!(login.validate().is_err());
        login.preferences.key_file.clear();
        login.preferences.passphrase = Some(String::new());
        assert!(login.validate().is_err());
    }
}
