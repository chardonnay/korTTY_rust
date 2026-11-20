//! Bounded external skill requests with scope-bound authentication.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use kortty_vault::Secret;
use ureq::http::{HeaderValue, header};
use url::Url;
use zeroize::Zeroizing;

use super::{ProviderProfile, within};
use crate::vault_window::VaultWindow;

pub(super) const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
const OPERATION_LIMIT: Duration = Duration::from_secs(30);
const CANCELLED: &str = "The external skill request was cancelled.";

pub(super) struct Network {
    agent: ureq::Agent,
    cancelled: Arc<AtomicBool>,
    credentials: BTreeMap<String, Secret>,
    vault_revision: Option<Weak<()>>,
}

pub(crate) struct Cancellation {
    agent: ureq::Agent,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.agent.clone().shutdown();
    }
}

impl Network {
    pub(super) fn new<'a>(
        profiles: impl Iterator<Item = &'a ProviderProfile>,
        vault: Option<&VaultWindow>,
    ) -> Result<Self, String> {
        let credentials: BTreeMap<_, _> = profiles
            .filter(|profile| profile.enabled)
            .filter_map(|profile| profile.vault_credential.as_ref())
            .filter_map(|id| {
                vault?
                    .skill_provider_credential(id)
                    .map(|secret| (id.clone(), secret))
            })
            .collect();
        let vault_revision = (!credentials.is_empty())
            .then(|| vault.and_then(VaultWindow::credential_revision))
            .flatten();
        Ok(Self {
            agent: crate::local_ai::service_agent(None, OPERATION_LIMIT)?,
            cancelled: Arc::new(AtomicBool::new(false)),
            credentials,
            vault_revision,
        })
    }

    pub(super) fn cancel_handle(&self) -> Cancellation {
        Cancellation {
            agent: self.agent.clone(),
            cancelled: Arc::clone(&self.cancelled),
        }
    }

    pub(super) fn operation(&self) -> Operation<'_> {
        Operation {
            network: self,
            deadline: Instant::now() + OPERATION_LIMIT,
        }
    }

    pub(super) fn vault_revision(&self) -> Option<Weak<()>> {
        self.vault_revision.clone()
    }

    fn vault_credential(&self, id: &str) -> Result<&Secret, String> {
        require_vault_revision(self.vault_revision.as_ref())?;
        self.credentials.get(id).ok_or_else(|| {
            "Unlock the vault and select a valid external skill provider credential.".into()
        })
    }

    pub(super) fn require_credential(&self, profile: &ProviderProfile) -> Result<(), String> {
        if let Some(id) = &profile.vault_credential {
            self.vault_credential(id)?;
        }
        Ok(())
    }
}

pub(super) struct Operation<'a> {
    network: &'a Network,
    deadline: Instant,
}

pub(super) fn require_policy() -> Result<(), String> {
    let policy = crate::clipboard_policy::ClipboardPolicy::load();
    if policy.problem.is_some() || !policy.ai_allowed {
        return Err("External skills are unavailable under the current AI policy.".into());
    }
    Ok(())
}

pub(in crate::ai_manager::skills) fn require_vault_revision(
    revision: Option<&Weak<()>>,
) -> Result<(), String> {
    if revision.is_some_and(|revision| revision.strong_count() == 0) {
        return Err("The vault changed. Unlock it and start a new external skill request.".into());
    }
    Ok(())
}

impl Operation<'_> {
    pub(super) fn json(
        &self,
        url: &Url,
        profile: Option<&ProviderProfile>,
        accept: &str,
    ) -> Result<serde_json::Value, String> {
        let bytes = self.get(url, profile, accept, MAX_JSON_BYTES)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| "The skill provider returned invalid JSON.".into())
    }

    pub(super) fn get(
        &self,
        url: &Url,
        profile: Option<&ProviderProfile>,
        accept: &str,
        limit: usize,
    ) -> Result<Vec<u8>, String> {
        require_policy()?;
        let remaining = self.remaining()?;
        let mut request = self
            .network
            .agent
            .get(url.as_str())
            .header(header::ACCEPT, accept)
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::USER_AGENT, "kortty-external-skills");
        if accept == super::github::ACCEPT {
            request = request.header("X-GitHub-Api-Version", "2022-11-28");
        }
        if let Some(profile) = profile
            && let Some(authorization) = self.authorization(profile, url)?
        {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        self.remaining()?;
        let response = request
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(remaining))
            .build()
            .call();
        self.remaining()?;
        require_policy()?;
        let mut response = response.map_err(|error| match error {
            ureq::Error::Timeout(_) => "The external skill provider timed out.",
            _ => "Could not contact the external skill provider.",
        })?;
        match response.status().as_u16() {
            200 => {}
            300..=399 => return Err("The external skill provider redirected the request.".into()),
            401 => {
                return Err("The external skill provider did not accept the credentials.".into());
            }
            403 => {
                return Err(
                    "The external skill provider denied access or limited requests.".into(),
                );
            }
            404 => return Err("The external skill source was not found.".into()),
            429 => return Err("The external skill provider is limiting requests.".into()),
            status => {
                return Err(format!(
                    "The external skill provider returned HTTP {status}."
                ));
            }
        }
        if response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|size| size > limit)
        {
            return Err("The external skill provider response exceeds its size limit.".into());
        }
        if accept.starts_with("text/markdown")
            && response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/html"))
        {
            return Err(
                "The source returned a web page. Enter the URL of the Markdown file.".into(),
            );
        }
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| "Could not read the external skill provider response.")?;
        self.remaining()?;
        require_policy()?;
        if body.len() > limit {
            return Err("The external skill provider response exceeds its size limit.".into());
        }
        if let Some(profile) = profile
            && let Ok(text) = std::str::from_utf8(&body)
        {
            self.require_no_credential_echo(profile, text)?;
        }
        Ok(body)
    }

    fn remaining(&self) -> Result<Duration, String> {
        if self.network.cancelled.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        require_vault_revision(self.network.vault_revision.as_ref())?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("The external skill provider timed out.".into());
        }
        Ok(remaining)
    }

    pub(super) fn require_no_credential_echo(
        &self,
        profile: &ProviderProfile,
        body: &str,
    ) -> Result<(), String> {
        self.with_credential(profile, |secret| {
            if secret.is_some_and(|secret| crate::local_ai::credential_echo(body, secret)) {
                return Err("The skill provider response echoed a credential.".into());
            }
            Ok(())
        })
    }

    fn with_credential<T>(
        &self,
        profile: &ProviderProfile,
        use_secret: impl FnOnce(Option<&str>) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(id) = &profile.vault_credential {
            let secret = self.network.vault_credential(id)?;
            return use_secret(Some(secret.as_str()));
        }
        if let Some(name) = profile.credential_name() {
            let secret = Zeroizing::new(std::env::var(name).map_err(
                |_| "The configured skill provider credential is unavailable or invalid.",
            )?);
            return use_secret(Some(&secret));
        }
        use_secret(None)
    }

    fn authorization(
        &self,
        profile: &ProviderProfile,
        url: &Url,
    ) -> Result<Option<HeaderValue>, String> {
        if profile.has_credential() && !profile.base()?.is_some_and(|base| within(url, &base)) {
            return Err("The skill provider credential does not authorize this URL.".into());
        }
        self.with_credential(profile, |secret| {
            let Some(secret) = secret else {
                return Ok(None);
            };
            let invalid =
                || "The configured skill provider credential is unavailable or invalid.".to_owned();
            let header = if profile.username.is_empty() {
                if secret.is_empty()
                    || secret.len() > 4096
                    || !secret.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err(invalid());
                }
                Zeroizing::new(format!("Bearer {secret}"))
            } else {
                if secret.is_empty() || secret.len() > 4096 || secret.chars().any(char::is_control)
                {
                    return Err(invalid());
                }
                let pair = Zeroizing::new(format!("{}:{secret}", profile.username));
                let encoded = Zeroizing::new(STANDARD.encode(pair.as_bytes()));
                Zeroizing::new(format!("Basic {}", encoded.as_str()))
            };
            let mut header = HeaderValue::from_str(&header).map_err(|_| invalid())?;
            header.set_sensitive(true);
            Ok(Some(header))
        })
    }
}
