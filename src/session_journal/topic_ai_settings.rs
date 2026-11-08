use std::io;

use serde::{Deserialize, Serialize};

use crate::{clipboard_policy::ClipboardPolicy, local_ai::Profile};

pub(crate) const MAX_ADDRESS_BYTES: usize = 128;
pub(crate) const MAX_MODEL_BYTES: usize = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) vision_enabled: bool,
    pub(crate) auto_screenshot_analysis: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            vision_enabled: false,
            auto_screenshot_analysis: true,
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn profile(&self) -> io::Result<Option<Profile>> {
        self.enabled.then(|| self.configured_profile()).transpose()
    }

    pub(crate) fn vision_profile(&self) -> io::Result<Option<Profile>> {
        if self.vision_enabled {
            self.profile()
        } else {
            Ok(None)
        }
    }

    /// The caller must additionally require this recording's consent to automatic image analysis.
    pub(crate) fn automatic_vision_profile(
        &self,
        policy: &ClipboardPolicy,
    ) -> io::Result<Option<Profile>> {
        if !vision_allowed(policy)
            || !policy
                .terminal_logging
                .journal_ai_screenshot_analysis()
                .unwrap_or(self.auto_screenshot_analysis)
        {
            return Ok(None);
        }
        self.vision_profile()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if !self.endpoint.is_empty() || !self.model.is_empty() {
            self.configured_profile()?;
        }
        Ok(())
    }

    fn configured_profile(&self) -> io::Result<Profile> {
        if self.endpoint.len() > MAX_ADDRESS_BYTES || self.endpoint.chars().any(char::is_control) {
            return Err(invalid("Enter a numeric loopback address and port."));
        }
        if self.model.len() > MAX_MODEL_BYTES || self.model.chars().any(char::is_control) {
            return Err(invalid("Enter the installed model name."));
        }
        Profile::new(&self.endpoint, &self.model).map_err(invalid)
    }
}

pub(crate) fn allowed(policy: &ClipboardPolicy) -> bool {
    policy.problem.is_none() && policy.session_journal_allowed && policy.ai_allowed
}

pub(crate) fn vision_allowed(policy: &ClipboardPolicy) -> bool {
    allowed(policy) && policy.terminal_logging.journal_ai_screenshot_analysis() != Some(false)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
