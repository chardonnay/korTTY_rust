//! Portable, inactive metadata from Java's local model registries.

use std::collections::{HashMap, HashSet};
use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::managed_models::{ModelBackend, Purpose};

use super::invalid;

pub(crate) const GGUF_MEMBER: &str = "llm/models.xml";
pub(crate) const MLX_MEMBER: &str = "llm/mlx-models.json";
const MAX_REGISTRY_BYTES: usize = 1024 * 1024;
const MAX_HINTS: usize = 384;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Kind {
    Gguf,
    Mlx,
}

impl Kind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Gguf => "GGUF",
            Self::Mlx => "MLX",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Metadata {
    Gguf {
        backend: ModelBackend,
        purpose: Purpose,
        context: u32,
        threads: u16,
        gpu_layers: i16,
        idle_minutes: u16,
    },
    Mlx {
        context: u32,
        idle_minutes: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantization: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Hint {
    pub(super) id: String,
    pub(super) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) published_at: Option<String>,
    pub(super) metadata: Metadata,
}

impl Hint {
    pub(super) fn kind(&self) -> Kind {
        match self.metadata {
            Metadata::Gguf { .. } => Kind::Gguf,
            Metadata::Mlx { .. } => Kind::Mlx,
        }
    }

    pub(super) fn details(&self) -> String {
        let context_value = |value: u32| {
            if value == 0 {
                "Model default".to_owned()
            } else {
                value.to_string()
            }
        };
        let idle_value = |value: u16| {
            if value == 0 {
                "Never unload".to_owned()
            } else {
                format!("{value} min idle")
            }
        };
        let mut details = match &self.metadata {
            Metadata::Gguf {
                backend,
                purpose,
                context,
                threads,
                gpu_layers,
                idle_minutes,
            } => format!(
                "{purpose} · {backend} · Context {} · CPU threads {} · GPU layers {} · {}",
                context_value(*context),
                if *threads == 0 {
                    "Auto".into()
                } else {
                    threads.to_string()
                },
                if *gpu_layers == -1 {
                    "Auto".into()
                } else {
                    gpu_layers.to_string()
                },
                idle_value(*idle_minutes),
            ),
            Metadata::Mlx {
                context,
                idle_minutes,
                quantization,
            } => {
                let mut details = format!(
                    "Context {} · {}",
                    context_value(*context),
                    idle_value(*idle_minutes)
                );
                if let Some(quantization) = quantization {
                    details.push_str(&format!(" · Quantization {quantization}"));
                }
                details
            }
        };
        if let Some(published_at) = &self.published_at {
            details.push_str(&format!(" · Published {published_at}"));
        }
        details
    }

    fn validate(&self) -> bool {
        if !safe_identifier(&self.id, 128)
            || self.name.is_empty()
            || self.name.trim() != self.name
            || self.name.chars().count() > 200
            || self.name.chars().any(char::is_control)
            || !self.published_at.as_deref().is_none_or(valid_date)
        {
            return false;
        }
        let (context, idle_minutes) = match &self.metadata {
            Metadata::Gguf {
                context,
                threads,
                gpu_layers,
                idle_minutes,
                ..
            } if *threads <= 1_024 && (-1..=10_000).contains(gpu_layers) => {
                (*context, *idle_minutes)
            }
            Metadata::Mlx {
                context,
                idle_minutes,
                quantization,
            } if quantization
                .as_deref()
                .is_none_or(|value| safe_identifier(value, 32)) =>
            {
                (*context, *idle_minutes)
            }
            _ => return false,
        };
        (context == 0 || (512..=2_097_152).contains(&context)) && idle_minutes <= 1_440
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub(super) struct Hints(Vec<Hint>);

impl Hints {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Hint> {
        self.0.iter()
    }

    pub(super) fn get(&self, kind: Kind, id: &str) -> Option<&Hint> {
        self.iter()
            .find(|hint| hint.kind() == kind && hint.id == id)
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        let mut keys = HashSet::new();
        if self.0.len() > MAX_HINTS
            || self
                .iter()
                .any(|hint| !hint.validate() || !keys.insert((hint.kind(), &hint.id)))
        {
            return Err(invalid("Invalid Java local model suggestions"));
        }
        Ok(())
    }

    pub(super) fn merge(&mut self, incoming: Self) -> Merge {
        let current = self
            .iter()
            .map(|hint| (hint.kind(), &hint.id))
            .collect::<HashSet<_>>();
        let mut full = 0;
        let mut additions = Vec::new();
        for hint in incoming.0 {
            if current.contains(&(hint.kind(), &hint.id)) {
                continue;
            }
            if self.0.len() + additions.len() >= MAX_HINTS {
                full += 1;
            } else {
                additions.push(hint);
            }
        }
        let added = additions.len();
        self.0.extend(additions);
        Merge { added, full }
    }
}

pub(super) fn require_review_policy() -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    policy.require_ai_user_model_files()?;
    if !policy.ai_allowed {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Managed policy disables AI model settings review",
        ));
    }
    Ok(())
}

pub(super) struct Merge {
    pub(super) added: usize,
    pub(super) full: usize,
}

pub(super) struct Import {
    pub(super) hints: Hints,
    pub(super) invalid: usize,
    pub(super) dates_omitted: usize,
}

#[derive(Default)]
struct Candidates {
    ids: HashMap<String, usize>,
    hints: Vec<Hint>,
    invalid: usize,
    dates_omitted: usize,
}

impl Candidates {
    fn claim(&mut self, id: &str) {
        let id = id.trim();
        if safe_identifier(id, 128) {
            *self.ids.entry(id.into()).or_default() += 1;
        }
    }

    fn add(&mut self, hint: Option<Hint>) {
        let Some(mut hint) = hint else {
            self.invalid += 1;
            return;
        };
        let bad_date = hint
            .published_at
            .as_deref()
            .is_some_and(|date| !valid_date(date));
        if bad_date {
            hint.published_at = None;
        }
        if hint.validate() {
            self.dates_omitted += usize::from(bad_date);
            self.hints.push(hint);
        } else {
            self.invalid += 1;
        }
    }

    fn finish(mut self) -> Import {
        let before = self.hints.len();
        self.hints.retain(|hint| self.ids.get(&hint.id) == Some(&1));
        self.hints.truncate(MAX_HINTS);
        Import {
            invalid: self.invalid + before - self.hints.len(),
            hints: Hints(self.hints),
            dates_omitted: self.dates_omitted,
        }
    }
}

pub(super) fn import(kind: Kind, bytes: &[u8]) -> io::Result<Import> {
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err(invalid("Java local model registry exceeds its size limit"));
    }
    match kind {
        Kind::Gguf => gguf(bytes),
        Kind::Mlx => mlx(bytes),
    }
}

fn gguf(bytes: &[u8]) -> io::Result<Import> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let xml =
        std::str::from_utf8(bytes).map_err(|_| invalid("Invalid Java GGUF registry encoding"))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Invalid Java GGUF registry XML"))?;
    let root = document.root_element();
    if !named(root, "llamaModels") || !matches!(scalar(root, "schemaVersion"), Ok(None | Some("1")))
    {
        return Err(invalid("Unsupported Java GGUF registry schema"));
    }
    let mut candidates = Candidates::default();
    for node in root.children().filter(|node| named(*node, "model")) {
        for id in node
            .children()
            .filter(|node| named(*node, "id"))
            .filter_map(|node| node.text())
        {
            candidates.claim(id);
        }
        candidates.add(gguf_hint(node));
    }
    Ok(candidates.finish())
}

fn gguf_hint(node: Node<'_, '_>) -> Option<Hint> {
    let backend = match scalar(node, "backend").ok()?.unwrap_or("AUTO") {
        "AUTO" => ModelBackend::Auto,
        "CPU" => ModelBackend::Cpu,
        "METAL" => ModelBackend::Metal,
        "VULKAN" => ModelBackend::Vulkan,
        _ => return None,
    };
    let purpose = match scalar(node, "purpose").ok()?.unwrap_or("CHAT") {
        "CHAT" => Purpose::Chat,
        "EMBEDDING" => Purpose::Embedding,
        _ => return None,
    };
    Some(Hint {
        id: scalar(node, "id").ok()??.into(),
        name: scalar(node, "displayName").ok()??.trim().into(),
        published_at: scalar(node, "publishedAt")
            .ok()?
            .map(|date| date.trim().into()),
        metadata: Metadata::Gguf {
            backend,
            purpose,
            context: scalar(node, "contextSize")
                .ok()?
                .unwrap_or("32768")
                .parse()
                .ok()?,
            threads: scalar(node, "threadCount")
                .ok()?
                .unwrap_or("0")
                .parse()
                .ok()?,
            gpu_layers: scalar(node, "gpuLayers")
                .ok()?
                .unwrap_or("-1")
                .parse()
                .ok()?,
            idle_minutes: scalar(node, "idleTimeoutMinutes")
                .ok()?
                .unwrap_or("10")
                .parse()
                .ok()?,
        },
    })
}

fn named(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn scalar<'a>(root: Node<'a, '_>, name: &str) -> Result<Option<&'a str>, ()> {
    let mut fields = root.children().filter(|node| named(*node, name));
    let Some(node) = fields.next() else {
        return Ok(None);
    };
    if fields.next().is_some()
        || node.children().count() != 1
        || !node.first_child().is_some_and(|child| child.is_text())
    {
        return Err(());
    }
    Ok(node.text())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MlxRegistry<'a> {
    schema_version: Option<u32>,
    #[serde(borrow)]
    models: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct MlxIdentity {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MlxSource {
    id: String,
    display_name: String,
    context_size: Option<u32>,
    idle_timeout_minutes: Option<u16>,
    quantization_label: Option<String>,
    published_at: Option<String>,
}

fn mlx(bytes: &[u8]) -> io::Result<Import> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let registry: MlxRegistry<'_> =
        serde_json::from_slice(bytes).map_err(|_| invalid("Invalid Java MLX registry JSON"))?;
    if registry.schema_version.unwrap_or(1) != 1 {
        return Err(invalid("Unsupported Java MLX registry schema"));
    }
    let mut candidates = Candidates::default();
    for raw in registry.models {
        if let Ok(identity) = serde_json::from_str::<MlxIdentity>(raw.get()) {
            candidates.claim(&identity.id);
        }
        let hint = serde_json::from_str::<MlxSource>(raw.get())
            .ok()
            .map(|model| Hint {
                id: model.id,
                name: model.display_name.trim().into(),
                published_at: model.published_at.map(|date| date.trim().into()),
                metadata: Metadata::Mlx {
                    context: model.context_size.unwrap_or(0),
                    idle_minutes: model.idle_timeout_minutes.unwrap_or(10),
                    quantization: model.quantization_label,
                },
            });
        candidates.add(hint);
    }
    Ok(candidates.finish())
}

fn safe_identifier(value: &str, limit: usize) -> bool {
    value.len() <= limit
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_date(value: &str) -> bool {
    value.len() <= 30 && value.ends_with('Z') && value.parse::<jiff::Timestamp>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_model_registries_reject_unsafe_schemas_and_untrusted_native_fields() {
        assert!(import(Kind::Gguf, br#"<!DOCTYPE llamaModels [<!ENTITY target SYSTEM "file:///private/model">]>
            <llamaModels><model><id>test</id><displayName>&target;</displayName></model></llamaModels>"#).is_err());
        assert!(
            import(
                Kind::Gguf,
                b"<llamaModels><schemaVersion>2</schemaVersion></llamaModels>"
            )
            .is_err()
        );
        assert!(import(Kind::Mlx, br#"{"schemaVersion":2,"models":[]}"#).is_err());
        assert!(
            serde_json::from_str::<Hints>(
                r#"[{"id":"source","name":"Test","metadata":{
            "kind":"mlx","context":0,"idle_minutes":10,"modelDirectory":"/not-authorized"
        }}]"#
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_model_claims_omit_all_matching_rows_and_invalid_dates_are_ignored() {
        let imported = import(
            Kind::Mlx,
            r#"{"schemaVersion":1,"models":[
          {"id":"duplicate","displayName":"First"},
          {"id":" duplicate ","displayName":false},
          {"id":"retained","displayName":"独立したモデル","publishedAt":"/archive-path"},
          {"id":"invalid-quantization","displayName":"Invalid","quantizationLabel":"../invalid"}
        ]}"#
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(imported.invalid, 3);
        assert_eq!(imported.dates_omitted, 1);
        assert_eq!(imported.hints.iter().count(), 1);
        let retained = imported.hints.get(Kind::Mlx, "retained").unwrap();
        assert!(retained.published_at.is_none());
        assert_eq!(retained.name, "独立したモデル");
    }
}
