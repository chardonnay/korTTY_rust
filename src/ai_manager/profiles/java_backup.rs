//! Java remote HTTPS AI profiles retained for a new destination review.

use std::collections::BTreeSet;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};

use super::{
    Catalog, ConnectionMode, MAX_PROFILES, ModelMode, Profile, ProfileId, Reasoning, Vision,
};
use crate::local_ai::{self, PromptPreset, RemoteProtocol, Tokenizer};
use crate::ssh_dialog::unsafe_text;

const MAX_XML_BYTES: usize = 4 * 1024 * 1024;
const MAX_XML_NODES: u32 = 65_536;

#[derive(Default)]
pub(in crate::ai_manager) struct Report {
    pub(in crate::ai_manager) imported: usize,
    pub(in crate::ai_manager) omitted: usize,
    pub(in crate::ai_manager) keys_omitted: usize,
}

pub(in crate::ai_manager) fn decode(bytes: &[u8]) -> io::Result<(Catalog, Report)> {
    if bytes.len() > MAX_XML_BYTES {
        return Err(invalid("Java global settings exceed their XML size limit"));
    }
    let xml = std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
        .map_err(|_| invalid("Java global settings XML must use UTF-8"))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Invalid Java global settings XML"))?;
    let root = document.root_element();
    if !named(root, "globalSettings") {
        return Err(invalid("Unexpected Java global settings XML root"));
    }
    let mut catalog = Catalog::default();
    let mut report = Report::default();
    let Some(wrapper) =
        child(root, "aiProfiles").map_err(|()| invalid("Duplicate Java AI profile list"))?
    else {
        return Ok((catalog, report));
    };
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for node in wrapper.children().filter(|node| named(*node, "profile")) {
        if node.children().any(|node| named(node, "encryptedApiKey")) {
            report.keys_omitted += 1;
        }
        match scalar(node, "id") {
            Ok(Some(id)) if !id.is_empty() && !ids.insert(id) => {
                return Err(invalid("Duplicate Java AI profile identity"));
            }
            Err(()) => {
                report.omitted += 1;
                continue;
            }
            _ => {}
        }
        let Ok(profile) = read_profile(node) else {
            report.omitted += 1;
            continue;
        };
        if catalog.profiles.len() == MAX_PROFILES
            || !names.insert(profile.name.to_ascii_lowercase())
        {
            report.omitted += 1;
            continue;
        }
        catalog.append_reviewed(profile)?;
        report.imported += 1;
    }
    Ok((catalog, report))
}

impl Catalog {
    fn append_reviewed(&mut self, mut profile: Profile) -> io::Result<()> {
        let id = self
            .last_id
            .checked_add(1)
            .filter(|id| !self.is_policy_reference(*id))
            .ok_or_else(|| invalid("AI profile identifiers are exhausted"))?;
        profile.id = ProfileId::from(id);
        self.profiles.push(profile);
        self.last_id = id;
        Ok(())
    }

    pub(in crate::ai_manager) fn merge_java_https(&mut self, incoming: Self) -> io::Result<usize> {
        let mut added = 0;
        for profile in incoming.profiles {
            if self.profiles.len() == MAX_PROFILES
                || self
                    .profiles
                    .iter()
                    .any(|local| local.name.eq_ignore_ascii_case(&profile.name))
            {
                continue;
            }
            self.append_reviewed(profile)?;
            added += 1;
        }
        Ok(added)
    }

    pub(in crate::ai_manager) fn validate_java_https(&self) -> io::Result<()> {
        if self.default.is_some()
            || self.policy_default.is_some()
            || self
                .profiles
                .iter()
                .any(|profile| !profile.is_java_https_draft())
        {
            return Err(invalid("Invalid Java HTTPS AI profile delta"));
        }
        Ok(())
    }
}

impl Profile {
    fn is_java_https_draft(&self) -> bool {
        self.restore_review
            && self.model_mode == ModelMode::Manual
            && local_ai::valid_model(&self.model)
            && !unsafe_text(&self.name)
            && !unsafe_text(&self.model)
            && remote_destination(&self.endpoint).is_ok_and(|(mode, destination)| {
                mode == self.connection_mode && destination == self.endpoint
            })
            && self.usage_budget == local_ai::UsageBudget::default()
            && self.validate().is_ok()
    }
}

fn read_profile(node: Node<'_, '_>) -> Result<Profile, ()> {
    if !matches!(scalar(node, "connectionMode")?, None | Some("HTTP_API"))
        || !matches!(scalar(node, "modelSelectionMode")?, None | Some("MANUAL"))
    {
        return Err(());
    }
    let name = scalar(node, "name")?
        .filter(|name| !unsafe_text(name))
        .ok_or(())?;
    let model = scalar(node, "model")?
        .filter(|model| local_ai::valid_model(model) && !unsafe_text(model))
        .ok_or(())?;
    let (connection_mode, endpoint) = remote_destination(scalar(node, "apiUrl")?.ok_or(())?)?;
    let profile = Profile {
        name: name.into(),
        model: model.into(),
        endpoint,
        connection_mode,
        restore_review: true,
        prompt_preset: Some(match scalar(node, "promptPreset")? {
            None | Some("AUTO") => PromptPreset::Auto,
            Some("GENERIC") => PromptPreset::Generic,
            Some("LLAMA") => PromptPreset::Llama,
            Some("QWEN") => PromptPreset::Qwen,
            Some("MISTRAL") => PromptPreset::Mistral,
            Some("GEMMA") => PromptPreset::Gemma,
            Some("DEEPSEEK") => PromptPreset::DeepSeek,
            Some("PHI") => PromptPreset::Phi,
            Some("GPT_OSS") => PromptPreset::GptOss,
            _ => return Err(()),
        }),
        vision: match scalar(node, "visionSupport")? {
            None | Some("AUTO") => Vision::Auto,
            Some("ENABLED") => Vision::Enabled,
            Some("DISABLED") => Vision::Disabled,
            _ => return Err(()),
        },
        reasoning: match scalar(node, "reasoningEffort")? {
            None | Some("DISABLED") => Reasoning::Default,
            Some("NONE") => Reasoning::None,
            Some("MINIMAL") => Reasoning::Minimal,
            Some("LOW") => Reasoning::Low,
            Some("MEDIUM") => Reasoning::Medium,
            Some("HIGH") => Reasoning::High,
            Some("XHIGH") => Reasoning::ExtraHigh,
            _ => return Err(()),
        },
        tokenizer: match scalar(node, "tokenizerType")? {
            None | Some("ESTIMATE") => Tokenizer::Estimate,
            Some("CL100K_BASE") => Tokenizer::Cl100kBase,
            Some("O200K_BASE") => Tokenizer::O200kBase,
            Some("P50K_BASE") => Tokenizer::P50kBase,
            Some("R50K_BASE") => Tokenizer::R50kBase,
            _ => return Err(()),
        },
        selection_limit: number(node, "maxSelectionChars")?
            .unwrap_or(Profile::default().selection_limit),
        timeout_minutes: number(node, "requestTimeoutMinutes")?,
        max_output_tokens: number(node, "maxOutputTokens")?,
        ..Profile::default()
    };
    if !profile.is_java_https_draft() {
        return Err(());
    }
    Ok(profile)
}

fn remote_destination(endpoint: &str) -> Result<(ConnectionMode, String), ()> {
    if unsafe_text(endpoint) || !endpoint.is_ascii() {
        return Err(());
    }
    let raw_path = endpoint
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, path)| path)
        .ok_or(())?;
    // URL parsing could silently remove dot segments. Java's saved route must stay exact.
    if raw_path.contains('%') || raw_path.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(());
    }
    let url = url::Url::parse(endpoint).map_err(|_| ())?;
    let path = url.path();
    let (mode, protocol, java_path) = if path.ends_with("/v1/messages") {
        (
            ConnectionMode::Anthropic,
            RemoteProtocol::Anthropic,
            path.to_owned(),
        )
    } else if path.ends_with("/chat/completions") {
        (
            ConnectionMode::RemoteOpenAi,
            RemoteProtocol::OpenAi,
            path.to_owned(),
        )
    } else if path.trim_end_matches('/').ends_with("/v1") {
        (
            ConnectionMode::RemoteOpenAi,
            RemoteProtocol::OpenAi,
            format!("{}/chat/completions", path.trim_end_matches('/')),
        )
    } else {
        return Err(());
    };
    let native = local_ai::remote_destination(endpoint, protocol).map_err(|_| ())?;
    if native != format!("{}{java_path}", url.origin().ascii_serialization()) {
        return Err(());
    }
    Ok((mode, native))
}

fn named(node: Node<'_, '_>, field: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.has_tag_name(field)
}

fn child<'a, 'input>(node: Node<'a, 'input>, field: &str) -> Result<Option<Node<'a, 'input>>, ()> {
    let mut found = node.children().filter(|child| named(*child, field));
    let first = found.next();
    if found.next().is_some() {
        return Err(());
    }
    Ok(first)
}

fn scalar<'a>(node: Node<'a, '_>, field: &str) -> Result<Option<&'a str>, ()> {
    let Some(node) = child(node, field)? else {
        return Ok(None);
    };
    let mut text = node
        .children()
        .filter(|child| child.is_element() || child.is_text());
    let value = text.next().map_or(Ok(""), |child| {
        child
            .is_text()
            .then(|| child.text().unwrap_or_default())
            .ok_or(())
    })?;
    if text.next().is_some() {
        return Err(());
    }
    Ok(Some(value.trim()))
}

fn number(node: Node<'_, '_>, field: &str) -> Result<Option<u32>, ()> {
    scalar(node, field)?
        .map(|value| value.parse().map_err(|_| ()))
        .transpose()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("fixtures/java-global-settings.xml");

    #[test]
    fn imports_only_exact_https_destinations_without_approvals_or_secrets() {
        let (catalog, report) = decode(FIXTURE).unwrap();
        assert_eq!(
            (report.imported, report.omitted, report.keys_omitted),
            (2, 2, 1)
        );
        let [openai, anthropic] = catalog.profiles.as_slice() else {
            panic!("expected two HTTPS profiles")
        };
        assert_eq!(openai.name, "Café HTTPS");
        assert_eq!(
            openai.endpoint,
            "https://example.test/openai/v1/chat/completions"
        );
        assert_eq!(openai.model, "family/model");
        assert_eq!(openai.prompt_preset, Some(PromptPreset::GptOss));
        assert_eq!(openai.reasoning, Reasoning::ExtraHigh);
        assert_eq!(openai.vision, Vision::Enabled);
        assert_eq!(openai.timeout_minutes, Some(0));
        assert_eq!(openai.max_output_tokens, Some(4096));
        assert_eq!(openai.selection_limit, 12000);
        assert_eq!(openai.tokenizer, Tokenizer::O200kBase);
        assert_eq!(anthropic.connection_mode, ConnectionMode::Anthropic);
        for profile in &catalog.profiles {
            assert!(profile.restore_review);
            assert!(profile.validate_connection().is_err());
            assert!(!profile.can_be_default());
        }
        assert!(catalog.default_profile().is_none());
        let json = serde_json::to_string(&catalog).unwrap();
        assert!(!json.contains("synthetic-source-api-key"));
        assert!(!json.contains("fixture-store"));
        assert!(!json.contains("untrusted-command"));
    }

    #[test]
    fn destination_approval_leaves_default_unset_until_explicitly_chosen() {
        let (mut catalog, _) = decode(FIXTURE).unwrap();
        assert!(catalog.validate().is_ok());
        assert!(!catalog.set_default(1));
        catalog.profiles[0].trusted_remote_endpoint = true;
        catalog.profiles[0].restore_review = false;
        assert!(catalog.validate().is_ok());
        assert!(catalog.default_profile().is_none());
        assert!(catalog.set_default(1));
        assert_eq!(catalog.default_profile().map(|profile| profile.id), Some(1));
    }

    #[test]
    fn existing_labels_and_default_win_and_replay_is_idempotent() {
        let (mut local, _) = decode(FIXTURE).unwrap();
        local.profiles[0].endpoint = "https://existing.test/v1/chat/completions".into();
        local.profiles[0].trusted_remote_endpoint = true;
        local.profiles[0].restore_review = false;
        local.profiles.truncate(1);
        assert!(local.set_default(1));
        let (incoming, _) = decode(FIXTURE).unwrap();
        assert_eq!(local.merge_java_https(incoming.clone()).unwrap(), 1);
        assert_eq!(local.merge_java_https(incoming).unwrap(), 0);
        assert_eq!(local.profiles.len(), 2);
        assert_eq!(
            local.profiles[0].endpoint,
            "https://existing.test/v1/chat/completions"
        );
        assert_eq!(local.profiles[1].id, 3);
        assert_eq!(local.default_profile().map(|profile| profile.id), Some(1));
        assert!(local.validate().is_ok());
    }

    #[test]
    fn rejects_urls_whose_path_or_authentication_would_change() {
        for url in [
            "https://example.test/arbitrary",
            "https://name:password@example.test/v1",
            "https://example.test/v1?secret=example",
            "https://example.test/v1/chat/completions#model",
            "https://example.test/v1/../v1/chat/completions",
            "https://example.test/a/%2e%2e/v1/chat/completions",
            "http://127.0.0.1:8080/v1/chat/completions",
        ] {
            assert!(
                remote_destination(url).is_err(),
                "unexpected destination {url}"
            );
        }
    }
}
