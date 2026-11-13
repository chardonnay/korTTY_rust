//! AI profiles declared by the installation policy.

use std::collections::BTreeSet;
use std::{fmt, io};

use pgp::crypto::hash::HashAlgorithm;
use toml_edit::{Array, Item};

use super::{allowed, invalid, string, trim_java, valid_policy_ai_profile_id};

const MAX_PROFILES: usize = 128;
const MAX_NAME_BYTES: usize = 160;
const MAX_ENDPOINT_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicyAiProvider {
    Anthropic,
    OpenAiCompatible,
    LmStudio,
    EmbeddedLlama,
    EmbeddedMlx,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PolicyAiProfile {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) provider: PolicyAiProvider,
    pub(crate) endpoint: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) encrypted_key: Option<String>,
}

impl PolicyAiProfile {
    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        let mut bytes = b"kortty policy AI profile v1\0".to_vec();
        bytes.push(match self.provider {
            PolicyAiProvider::Anthropic => 0,
            PolicyAiProvider::OpenAiCompatible => 1,
            PolicyAiProvider::LmStudio => 2,
            PolicyAiProvider::EmbeddedLlama => 3,
            PolicyAiProvider::EmbeddedMlx => 4,
        });
        for value in [
            Some(self.id.as_str()),
            Some(self.name.as_str()),
            self.endpoint.as_deref(),
            self.model.as_deref(),
            self.encrypted_key.as_deref(),
        ] {
            if let Some(value) = value {
                bytes.push(1);
                bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
                bytes.extend_from_slice(value.as_bytes());
            } else {
                bytes.push(0);
            }
        }
        HashAlgorithm::Sha256
            .digest(&bytes)
            .expect("SHA-256 is supported")
            .try_into()
            .expect("SHA-256 has 32 bytes")
    }
}

impl fmt::Debug for PolicyAiProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PolicyAiProfile")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

pub(super) fn parse(item: Option<Item>) -> io::Result<Vec<PolicyAiProfile>> {
    let Some(item) = item.filter(|item| !item.as_array().is_some_and(Array::is_empty)) else {
        return Ok(Vec::new());
    };
    let tables = item
        .into_array_of_tables()
        .map_err(|_| invalid("ai-profile must be an array of tables"))?;
    if tables.len() > MAX_PROFILES {
        return Err(invalid(format!(
            "The policy may declare at most {MAX_PROFILES} AI profiles"
        )));
    }
    let mut ids = BTreeSet::new();
    let mut profiles = Vec::with_capacity(tables.len());
    for (index, table) in tables.iter().enumerate() {
        let scope = format!("ai-profile #{}", index + 1);
        allowed(
            table,
            &scope,
            &[
                "id",
                "name",
                "provider",
                "endpoint",
                "model",
                "api-key-encrypted",
            ],
        )?;
        let optional = |key, max| {
            let value = string(table, &scope, key)?
                .map(trim_java)
                .filter(|value| !value.trim().is_empty());
            if value.is_some_and(|value| {
                value.len() > max
                    || value.chars().any(|ch| {
                        ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{feff}')
                    })
            }) {
                return Err(invalid(format!(
                    "{scope}.{key} must be bounded single-line text"
                )));
            }
            Ok(value)
        };
        let required = |key, max| {
            optional(key, max)?
                .ok_or_else(|| invalid(format!("{scope}.{key} must be non-empty text")))
        };
        // Java uses case-sensitive identifiers with a literal policy- prefix.
        let id = required("id", kortty_jobs::MAX_POLICY_PROFILE_ID_BYTES)?;
        if !valid_policy_ai_profile_id(id) {
            return Err(invalid(format!(
                "{scope}.id must start with policy- and have a suffix"
            )));
        }
        if !ids.insert(id) {
            return Err(invalid(format!(
                "{scope}.id duplicates a managed AI profile"
            )));
        }
        let provider = match required("provider", 32)?.to_ascii_lowercase().as_str() {
            "anthropic" => PolicyAiProvider::Anthropic,
            "openai-compatible" => PolicyAiProvider::OpenAiCompatible,
            "lm-studio" => PolicyAiProvider::LmStudio,
            "embedded-llama" => PolicyAiProvider::EmbeddedLlama,
            "embedded-mlx" => PolicyAiProvider::EmbeddedMlx,
            _ => return Err(invalid(format!("{scope}.provider is unsupported"))),
        };
        let encrypted_key = string(table, &scope, "api-key-encrypted")?
            .map(trim_java)
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                kortty_control::policy_value::validate_envelope(value).map_err(|_| {
                    invalid(format!(
                        "{scope}.api-key-encrypted must be a valid kortty-enc:v1: envelope"
                    ))
                })?;
                Ok::<_, io::Error>(value.to_owned())
            })
            .transpose()?;
        profiles.push(PolicyAiProfile {
            id: id.to_owned(),
            name: required("name", MAX_NAME_BYTES)?.to_owned(),
            provider,
            endpoint: optional("endpoint", MAX_ENDPOINT_BYTES)?.map(str::to_owned),
            model: optional("model", crate::local_ai::MAX_MODEL_BYTES)?.map(str::to_owned),
            encrypted_key,
        });
    }
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;
    use crate::clipboard_policy::{ClipboardPolicy, parse_with_identity};

    const META: &str = "[meta]\nschema-version = 1\n";

    fn envelope(byte: u8) -> String {
        format!("kortty-enc:v1:{}", STANDARD.encode([byte; 29]))
    }

    #[test]
    fn managed_ai_profile_policy_projects_all_java_providers_across_scopes() {
        let credential = envelope(0);
        let source = format!(
            r#"{META}
[[rule]]
users = ["viewer"]
[rule.ai-profiles]
allow-create = false

[[ai-profile]]
id = "  policy-Cloud  "
name = "  First managed profile  "
provider = " AnThRoPiC "
model = "example-chat"
api-key-encrypted = "{credential}"

[[ai-profile]]
id = "policy-cloud"
name = "Second profile"
provider = "openai-compatible"
endpoint = " https://example.invalid/v1 "

[[ai-profile]]
id = "policy-local"
name = "Local chat"
provider = "lm-studio"
endpoint = "http://127.0.0.1:1234"
model = ""

[[ai-profile]]
id = "policy-llama"
name = "Local GGUF"
provider = "embedded-llama"
model = "policy-acme-gguf"

[[ai-profile]]
id = "policy-modèle"
name = "Local MLX"
provider = "embedded-mlx"
model = "policy-acme-mlx"
"#
        );
        let read = |user| parse_with_identity(&source, None, || Ok(user), || Ok(&[])).unwrap();
        let viewer = read("viewer");
        let author = read("author");
        assert!(!viewer.ai_profile_creation_allowed());
        assert!(author.ai_profile_creation_allowed());
        let profiles = viewer.ai_profiles();
        assert_eq!(profiles, author.ai_profiles());
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.provider)
                .collect::<Vec<_>>(),
            [
                PolicyAiProvider::Anthropic,
                PolicyAiProvider::OpenAiCompatible,
                PolicyAiProvider::LmStudio,
                PolicyAiProvider::EmbeddedLlama,
                PolicyAiProvider::EmbeddedMlx,
            ]
        );
        assert_eq!(profiles[0].id, "policy-Cloud");
        assert_eq!(profiles[0].name, "First managed profile");
        assert_eq!(
            profiles[0].encrypted_key.as_deref(),
            Some(credential.as_str())
        );
        assert_eq!(profiles[0].endpoint, None);
        assert_eq!(
            profiles[1].endpoint.as_deref(),
            Some("https://example.invalid/v1")
        );
        assert_eq!(profiles[2].model, None);
        assert_eq!(profiles[4].id, "policy-modèle");
        assert!(
            ClipboardPolicy::lockdown("invalid policy".into())
                .ai_profiles()
                .is_empty()
        );
    }

    #[test]
    fn managed_ai_profile_policy_rejects_invalid_or_duplicate_definitions() {
        let profile = "[[ai-profile]]\nid = 'policy-test'\nname = 'Test'\nprovider = 'lm-studio'\n";
        let parse = |suffix: &str| {
            parse_with_identity(
                &format!("{META}{suffix}"),
                None,
                || Ok("tester"),
                || Ok(&[]),
            )
        };
        for bad in [
            profile.replace("policy-test", "regular-test"),
            profile.replace("policy-test", "policy-"),
            profile.replace("policy-test", "policy-\u{2028}test"),
            profile.replace("lm-studio", "ollama"),
            profile.replace("name = 'Test'", "name = 123"),
            profile.replace("name = 'Test'", "name = '  '"),
            format!("{profile}endpoint = true\n"),
            format!("{profile}api-key-encrypted = 'plaintext-fixture'\n"),
            format!("{profile}api-key-encrypted = 'kortty-enc:v1:not-base64-fixture'\n"),
            format!("{profile}credential-env = 'KEY'\n"),
            format!(
                "{profile}model = '{}'\n",
                "m".repeat(crate::local_ai::MAX_MODEL_BYTES + 1)
            ),
            format!("{profile}{profile}"),
            "[ai-profile]\nid = 'policy-test'\n".to_owned(),
        ] {
            assert!(parse(&bad).is_err());
        }
        let oversized = (0..=MAX_PROFILES)
            .map(|index| profile.replace("policy-test", &format!("policy-{index}")))
            .collect::<String>();
        assert!(parse(&oversized).is_err());
        assert!(parse("").unwrap().ai_profiles().is_empty());
        let inline =
            "ai-profile = [{ id = 'policy-inline', name = 'Inline', provider = 'anthropic' }]\n";
        let source = format!("{inline}{META}");
        let inline = parse_with_identity(&source, None, || Ok("tester"), || Ok(&[])).unwrap();
        assert_eq!(inline.ai_profiles()[0].id, "policy-inline");
    }

    #[test]
    fn managed_ai_profile_policy_binds_exact_definition_without_debug_secrets() {
        let initial = PolicyAiProfile {
            id: "policy-proof".into(),
            name: "Private fixture title".into(),
            provider: PolicyAiProvider::OpenAiCompatible,
            endpoint: Some("https://private.example.invalid/v1".into()),
            model: Some("private-model-fixture".into()),
            encrypted_key: Some(envelope(0)),
        };
        let fingerprint = initial.fingerprint();
        assert_eq!(fingerprint, initial.clone().fingerprint());
        let mut changed = initial.clone();
        changed.encrypted_key = Some(envelope(1));
        assert_ne!(fingerprint, changed.fingerprint());
        changed = initial.clone();
        changed.encrypted_key = None;
        assert_ne!(fingerprint, changed.fingerprint());
        changed = initial.clone();
        changed.endpoint = Some("https://new.example.invalid/v1".into());
        assert_ne!(fingerprint, changed.fingerprint());
        let diagnostic = format!("{initial:?}");
        assert!(diagnostic.contains("OpenAiCompatible"));
        assert!(!diagnostic.contains("private"));
        assert!(!diagnostic.contains("kortty-enc"));
        assert!(!diagnostic.contains("policy-proof"));
    }
}
