//! In-memory masking of a reviewed provider credential.

use zeroize::Zeroizing;

use crate::local_ai::Profile;
use crate::terminal_logging::{RedactionSecrets, Redactor};

#[derive(Clone)]
pub(crate) struct ProviderMask {
    credential: Option<Zeroizing<String>>,
}

impl ProviderMask {
    pub(crate) fn capture(profile: &Profile) -> Self {
        Self {
            credential: profile
                .is_remote()
                .then(|| profile.completion_secret())
                .flatten(),
        }
    }

    pub(crate) fn apply(&self, redactor: &Redactor) -> Result<Redactor, &'static str> {
        let Some(credential) = &self.credential else {
            return Ok(redactor.clone());
        };
        RedactionSecrets::with_secret(None, credential)
            .and_then(|secrets| redactor.with_overlay(&secrets))
            .map_err(|_| "The HTTPS provider credential could not be masked.")
    }

    pub(crate) fn check(&self, profile: &Profile) -> Result<(), &'static str> {
        if profile.is_remote() && profile.completion_secret() != self.credential {
            return Err("The HTTPS provider credential changed. Review the request again.");
        }
        Ok(())
    }

    pub(crate) fn check_text(&self, text: &str) -> Result<(), &'static str> {
        if self
            .credential
            .as_ref()
            .is_some_and(|secret| text.contains(secret.as_str()))
        {
            return Err("The reviewed model request contains a provider credential.");
        }
        Ok(())
    }
}
