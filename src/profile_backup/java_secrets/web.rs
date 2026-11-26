use std::io;

use kortty_vault::{
    CredentialAddition, CredentialInput, CredentialKind, MAX_WEB_CREDENTIAL_BYTES, Vault,
    WebProvider, valid_web_credential,
};
use zeroize::Zeroizing;

use super::{JavaSecrets, parse_xml, scalar};

const MAX_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENCODED_BYTES: usize = super::SALT_BYTES.div_ceil(3) * 4
    + 1
    + (MAX_WEB_CREDENTIAL_BYTES + super::ENVELOPE_OVERHEAD).div_ceil(3) * 4;

const FIELDS: [(WebProvider, &str); 3] = [
    (WebProvider::Tavily, "encryptedAiTavilyApiKey"),
    (WebProvider::BrightData, "encryptedAiBrightDataApiToken"),
    (WebProvider::Brave, "encryptedAiBraveSearchApiKey"),
];

#[derive(Default)]
pub(in crate::profile_backup) struct Source {
    fields: Vec<(WebProvider, Zeroizing<String>)>,
}

#[derive(Default)]
pub(crate) struct Summary {
    pub(crate) ready: Vec<WebProvider>,
    pub(crate) preserved: usize,
    pub(crate) omitted: usize,
    pub(crate) blocked_by_policy: bool,
}

impl Source {
    pub(in crate::profile_backup) fn from_settings(bytes: &[u8]) -> io::Result<(Self, usize)> {
        let document = parse_xml(bytes, MAX_SETTINGS_BYTES, "globalSettings")?;
        let root = document.root_element();
        let mut fields = Vec::new();
        let mut omitted = 0;
        for (provider, field) in FIELDS {
            match scalar(root, field) {
                Ok(Some(value)) if value.len() > MAX_ENCODED_BYTES => omitted += 1,
                Ok(Some(value)) if !value.is_empty() => {
                    fields.push((provider, Zeroizing::new(value.to_owned())));
                }
                Err(()) => omitted += 1,
                Ok(_) => {}
            }
        }
        Ok((Self { fields }, omitted))
    }

    pub(in crate::profile_backup) fn len(&self) -> usize {
        self.fields.len()
    }

    pub(in crate::profile_backup) fn prepare(
        &self,
        secrets: &JavaSecrets<'_>,
        vault: &Vault,
        additions: &mut Vec<CredentialAddition>,
        has_authenticated_ssh_secret: bool,
    ) -> io::Result<Summary> {
        let mut summary = Summary::default();
        let policy_allows_import = crate::local_ai::web_search::require_policy().is_ok();
        let mut authenticated = has_authenticated_ssh_secret;
        let mut failed_authentication = false;
        for (provider, encoded) in &self.fields {
            if vault.get(provider.id()).is_some() {
                summary.preserved += 1;
                continue;
            }
            if !policy_allows_import {
                summary.omitted += 1;
                summary.blocked_by_policy = true;
                continue;
            }
            let secret = match secrets.decrypt_field_with_limit(encoded, MAX_WEB_CREDENTIAL_BYTES) {
                Ok(secret) => {
                    authenticated = true;
                    secret
                }
                Err(error) => {
                    failed_authentication |= error.kind() == io::ErrorKind::PermissionDenied;
                    summary.omitted += 1;
                    continue;
                }
            };
            if !valid_web_credential(&secret) {
                summary.omitted += 1;
                continue;
            }
            additions.push(CredentialAddition {
                input: CredentialInput {
                    kind: CredentialKind::WebProvider(*provider),
                    label: provider.label().into(),
                    ..CredentialInput::default()
                },
                secret,
                webhook: None,
            });
            summary.ready.push(*provider);
        }
        if secrets.master_key.is_none() && failed_authentication && !authenticated {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The Java KorTTY master password could not authenticate any web provider credentials.",
            ));
        }
        Ok(summary)
    }
}
