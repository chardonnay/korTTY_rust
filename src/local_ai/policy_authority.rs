//! Exact installed policy and model source bound to an embedded profile.

use std::io;
use std::path::Path;

use pgp::crypto::hash::HashAlgorithm;

use crate::clipboard_policy::{ClipboardPolicy, PolicyModel, PolicyModelRuntime};

use super::policy_models::{self, ModelIdentity};

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PolicyModelAuthority {
    definition: PolicyModel,
    identity: ModelIdentity,
}

impl PolicyModelAuthority {
    pub(crate) fn resolve(root: &Path, id: &str, runtime: PolicyModelRuntime) -> io::Result<Self> {
        Self::resolve_from(&ClipboardPolicy::load(), id, runtime, |definition| {
            policy_models::model_identity(root, definition)
        })
    }

    fn resolve_from(
        policy: &ClipboardPolicy,
        id: &str,
        runtime: PolicyModelRuntime,
        identity: impl FnOnce(&PolicyModel) -> io::Result<ModelIdentity>,
    ) -> io::Result<Self> {
        let definition = policy
            .ai_runtime_models()
            .iter()
            .find(|definition| definition.id == id && definition.runtime == runtime)
            .ok_or_else(unavailable)?;
        Ok(Self {
            definition: definition.clone(),
            identity: identity(definition)?,
        })
    }

    pub(crate) fn definition(&self) -> &PolicyModel {
        &self.definition
    }

    pub(super) fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    pub(super) fn require_current(&self, policy: &ClipboardPolicy) -> io::Result<()> {
        if policy.problem.is_none()
            && policy.ai_allowed
            && policy.ai_runtime_models().contains(&self.definition)
        {
            Ok(())
        } else {
            Err(unavailable())
        }
    }

    pub(super) fn require_current_source(&self, root: &Path) -> io::Result<()> {
        self.require_current(&ClipboardPolicy::load())?;
        if policy_models::model_identity(root, &self.definition)? == self.identity {
            Ok(())
        } else {
            Err(unavailable())
        }
    }

    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        let mut bytes = b"kortty policy model v1\0".to_vec();
        bytes.push(match self.definition.runtime {
            PolicyModelRuntime::Gguf => 0,
            PolicyModelRuntime::Mlx => 1,
        });
        for value in [
            &self.definition.id,
            &self.definition.name,
            &self.definition.source,
        ] {
            bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes.extend_from_slice(&self.identity.fingerprint());
        if let Some(digest) = self.definition.digest {
            bytes.extend_from_slice(b"\0declared-sha256\0");
            bytes.extend_from_slice(&digest);
        }
        if let Some(path) = &self.definition.protected_cache {
            bytes.extend_from_slice(b"\0protected-cache\0");
            bytes.extend_from_slice(&(path.len() as u64).to_le_bytes());
            bytes.extend_from_slice(path.as_bytes());
        }
        HashAlgorithm::Sha256
            .digest(&bytes)
            .expect("SHA-256 is supported")
            .try_into()
            .expect("SHA-256 has 32 bytes")
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "The policy-provided model changed or is unavailable. Reopen the profile or review the request again.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_model(source: &str, runtime: &str) -> ClipboardPolicy {
        crate::clipboard_policy::parse(&format!(
            r#"
[meta]
schema-version = 1

[[rule]]
[rule.ai-runtime]
allow-user-models = false

[[ai-runtime.model]]
name = "Fixture Model"
runtime = "{runtime}"
source = "{source}"
"#
        ))
        .unwrap()
    }

    #[test]
    fn installed_model_authority_binds_definition_and_source_identity() {
        let original = policy_model("/managed/model-a.gguf", "llama");
        let rebound = policy_model("/managed/model-b.gguf", "llama");
        let wrong_runtime = policy_model("/managed/model-a.gguf", "mlx");
        let resolve = |policy: &ClipboardPolicy, tag| {
            PolicyModelAuthority::resolve_from(
                policy,
                "policy-fixture-model",
                PolicyModelRuntime::Gguf,
                |_| Ok(ModelIdentity::fixture(tag)),
            )
        };
        assert!(original.require_ai_user_model_files().is_err());
        let reviewed = resolve(&original, 1).unwrap();
        assert!(reviewed.require_current(&original).is_ok());
        for changed in [&rebound, &wrong_runtime, &ClipboardPolicy::default()] {
            assert_eq!(
                reviewed.require_current(changed).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert!(resolve(&wrong_runtime, 1).is_err());
        assert!(resolve(&ClipboardPolicy::default(), 1).is_err());
        assert_eq!(
            reviewed.fingerprint(),
            resolve(&original, 1).unwrap().fingerprint()
        );
        assert_ne!(
            reviewed.fingerprint(),
            resolve(&original, 2).unwrap().fingerprint()
        );
        assert_ne!(
            reviewed.fingerprint(),
            resolve(&rebound, 1).unwrap().fingerprint()
        );
    }

    #[test]
    fn installed_model_policy_revocation_refuses_a_cached_embedded_transport() {
        use crate::local_ai::{
            Client, Connection, DEFAULT_TIMEOUT, Error, Profile, ProfileOptions, Transport,
        };

        let configured = policy_model("/managed/model-a.gguf", "llama");
        let rebound = || policy_model("/managed/model-b.gguf", "llama");
        let root = crate::snippets::tests::Profile::new();
        let mut profile = Profile::managed(
            root.0.clone(),
            "0123456789abcdef0123456789abcdef",
            ProfileOptions::default(),
        )
        .unwrap();
        profile.model = "policy-fixture-model".into();
        profile.policy_model = Some(
            PolicyModelAuthority::resolve_from(
                &configured,
                &profile.model,
                PolicyModelRuntime::Gguf,
                |_| Ok(ModelIdentity::fixture(1)),
            )
            .unwrap(),
        );
        let client = Client::new(profile);
        assert!(matches!(
            client.transport_with_policy(None, rebound),
            Err(Error::Review(_))
        ));
        assert!(client.transport.get().is_none());
        assert!(
            client
                .transport
                .set(Ok(Transport {
                    connection: Connection::new("127.0.0.1:1".parse().unwrap(), DEFAULT_TIMEOUT),
                    lease: None,
                }))
                .is_ok()
        );
        assert!(client.transport_with_policy(None, || configured).is_ok());
        assert!(matches!(
            client.transport_with_policy(None, rebound),
            Err(Error::Review(_))
        ));
        assert!(matches!(
            client.transport_with_policy(None, ClipboardPolicy::default),
            Err(Error::Review(_))
        ));
    }
}
