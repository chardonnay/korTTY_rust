//! Exact organization, global, and target-scoped memory for reviewed requests.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::clipboard_policy::AiMemoryPolicy;
use crate::terminal_logging::Redactor;
use crate::workspace::terminal_ai::mask;

use super::{Configuration, MemoryPrompt};

pub(crate) const CHANGED: &str = "The reviewed AI memory changed. Review the request again.";

/// The saved approval contains a revision only. Reviewed memory remains in private storage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct Binding([u8; 32]);

#[derive(Clone)]
pub(crate) struct Memory {
    root: Option<PathBuf>,
    connection: Option<u64>,
    include_personal: bool,
    include_external: bool,
    prompt: Option<Arc<MemoryPrompt>>,
    binding: Option<Binding>,
}

impl Memory {
    pub(crate) fn capture(
        config: &Configuration,
        policy: &AiMemoryPolicy,
        connection: u64,
        redactor: &Redactor,
    ) -> Result<Self, String> {
        Self::masked(config, policy, Some(connection), true, |value| {
            mask::prompt(redactor, value)
        })
    }

    pub(crate) fn global(
        config: &Configuration,
        policy: &AiMemoryPolicy,
        redactor: &Redactor,
    ) -> Result<Self, String> {
        Self::masked(config, policy, None, true, |value| {
            mask::prompt(redactor, value)
        })
    }

    pub(crate) fn masked(
        config: &Configuration,
        policy: &AiMemoryPolicy,
        connection: Option<u64>,
        include_personal: bool,
        mask: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        Self::masked_with_options(config, policy, connection, include_personal, false, mask)
    }

    pub(crate) fn masked_for_interactive_review(
        config: &Configuration,
        policy: &AiMemoryPolicy,
        connection: Option<u64>,
        include_personal: bool,
        mask: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        Self::masked_with_options(
            config,
            policy,
            connection,
            include_personal,
            include_personal,
            mask,
        )
    }

    fn masked_with_options(
        config: &Configuration,
        policy: &AiMemoryPolicy,
        connection: Option<u64>,
        include_personal: bool,
        include_external: bool,
        mask: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        let root = config.saved_root();
        let personal_root = root.filter(|_| include_personal);
        let prompt =
            config.load_memory(personal_root, policy, connection, include_external, mask)?;
        let binding = prompt
            .as_ref()
            .map(|prompt| prompt.reviewed_revision(connection).map(Binding))
            .transpose()?;
        Ok(Self {
            root: root.map(ToOwned::to_owned),
            connection,
            include_personal,
            include_external,
            prompt: prompt.map(Arc::new),
            binding,
        })
    }

    pub(crate) fn text(&self) -> Option<&str> {
        self.prompt().map(MemoryPrompt::text)
    }

    pub(crate) fn prompt(&self) -> Option<&MemoryPrompt> {
        self.prompt.as_deref()
    }

    pub(crate) fn system<'a>(&self, system: &'a str, suffix: Option<&str>) -> Cow<'a, str> {
        let Some(memory) = self.text() else {
            return Cow::Borrowed(system);
        };
        let (body, suffix) = suffix
            .and_then(|suffix| system.strip_suffix(suffix).map(|body| (body, suffix)))
            .unwrap_or((system, ""));
        Cow::Owned(format!("{body}{memory}{suffix}"))
    }

    pub(crate) fn binding(&self) -> Option<Binding> {
        self.binding
    }

    pub(crate) fn connection(&self) -> Option<u64> {
        self.connection
    }

    pub(crate) fn includes_connection(&self) -> bool {
        self.prompt
            .as_deref()
            .is_some_and(MemoryPrompt::includes_connection)
    }

    pub(crate) fn require_current(
        &self,
        config: &Configuration,
        policy: &AiMemoryPolicy,
        redactor: &Redactor,
    ) -> Result<(), String> {
        self.require_current_with(config, policy, |value| mask::prompt(redactor, value))
    }

    pub(crate) fn require_current_with(
        &self,
        config: &Configuration,
        policy: &AiMemoryPolicy,
        mask: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        if config.saved_root() != self.root.as_deref() {
            return Err(CHANGED.into());
        }
        if let Some(prompt) = self.prompt().filter(|prompt| prompt.includes_external()) {
            prompt.require_current(policy)?;
        }
        let current = Self::masked_with_options(
            config,
            policy,
            self.connection,
            self.include_personal,
            self.include_external,
            mask,
        )?;
        if current.binding != self.binding {
            return Err(CHANGED.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_files::Directory;
    use crate::terminal_logging::{Policy, Secrets};

    #[test]
    fn scheduled_memory_masks_and_binds_only_the_selected_connection() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let memory = Directory::open(&root.join("memory"), true).unwrap();
        memory
            .publish_copy("MEMORY.md", b"Use approved maintenance windows.\n")
            .unwrap();
        let hosts = memory.private_child("connections", true).unwrap();
        hosts
            .publish_copy("19.md", b"Use staging and credential-value-one.\n")
            .unwrap();
        hosts
            .publish_copy("20.md", b"Inspect the production queue.\n")
            .unwrap();
        let config = Configuration::load_fixture_from(&root).unwrap();
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[organization-memory]]\ntitle = 'Approved scope'\ncontent = 'Observe change freezes.'",
        )
        .unwrap()
        .ai_memory;
        let secrets = Secrets::default();
        secrets.add("credential-value-one");
        secrets.add("credential-value-two");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
        let staging = Memory::capture(&config, &policy, 19, &redactor).unwrap();
        let production = Memory::capture(&config, &policy, 20, &redactor).unwrap();
        let global = Memory::global(&config, &policy, &redactor).unwrap();
        let text = staging.text().unwrap();
        assert!(text.contains("Observe change freezes."));
        assert!(text.contains("Use approved maintenance windows."));
        assert!(text.contains("Use staging and ***."));
        assert!(!text.contains("credential-value-one"));
        assert!(!text.contains("Inspect the production queue."));
        assert!(
            production
                .text()
                .unwrap()
                .contains("Inspect the production queue.")
        );
        assert!(!global.text().unwrap().contains("Use staging"));
        assert!(staging.includes_connection() && !global.includes_connection());
        assert_eq!(staging.connection(), Some(19));
        assert_ne!(production.binding(), staging.binding());
        assert_ne!(global.binding(), staging.binding());
        std::fs::write(
            root.join("memory/connections/19.md"),
            "Use staging and credential-value-two.\n",
        )
        .unwrap();
        let changed = Memory::capture(&config, &policy, 19, &redactor).unwrap();
        assert_eq!(changed.text(), staging.text());
        assert_ne!(changed.binding(), staging.binding());
        assert!(
            staging
                .require_current(&config, &policy, &redactor)
                .is_err()
        );
        assert!(
            production
                .require_current(&config, &policy, &redactor)
                .is_ok()
        );
    }

    #[test]
    fn scheduled_memory_rechecks_previous_absence_and_managed_denial() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let config = Configuration::load_fixture_from(&root).unwrap();
        let policy = AiMemoryPolicy::default();
        let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default());
        let absent = Memory::capture(&config, &policy, 19, &redactor).unwrap();
        assert!(absent.binding().is_none());
        let memory = Directory::open(&root.join("memory"), true).unwrap();
        memory
            .publish_copy("MEMORY.md", b"Use metric units.\n")
            .unwrap();
        assert!(absent.require_current(&config, &policy, &redactor).is_err());
        let approved = Memory::capture(&config, &policy, 19, &redactor).unwrap();
        let denied = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.features]\nai-memory = 'deny'",
        )
        .unwrap()
        .ai_memory;
        assert!(
            approved
                .require_current(&config, &denied, &redactor)
                .is_err()
        );
    }
}
