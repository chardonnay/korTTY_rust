//! Descriptive Hub search metadata. Artifact review reads the immutable file list separately.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use super::{Format, MAX_FILES, Repository, RepositoryWire, license_label, valid_file_path};
use crate::local_ai::managed_models::{MAX_MODEL_BYTES, split};

pub(super) const EXPANSIONS: [&str; 5] = ["gguf", "siblings", "tags", "createdAt", "config"];

static QUANTIZATION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:^|[-_.])((?:TQ|IQ|Q)[0-9]+(?:_[A-Z0-9]+)*|BF16|F(?:16|32|64))(?:$|[-.])",
    )
    .expect("fixed GGUF quantization pattern")
});

static MLX_QUANTIZATION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?:^|[-_])([0-9]{1,2})bit(?:$|[-_])")
        .expect("fixed MLX quantization pattern")
});

#[derive(Clone, Debug)]
pub(crate) struct ModelListing {
    pub(crate) repository: Repository,
    pub(crate) format: Format,
    pub(crate) architecture: Option<String>,
    pub(crate) quantizations: Vec<String>,
    pub(crate) license: Option<String>,
    pub(crate) bytes: Option<u64>,
    pub(crate) context: Option<u64>,
    pub(crate) created: Option<Timestamp>,
}

impl ModelListing {
    #[cfg(test)]
    pub(crate) fn for_test(repository: Repository) -> Self {
        Self {
            repository,
            format: Format::Gguf,
            architecture: None,
            quantizations: Vec::new(),
            license: None,
            bytes: None,
            context: None,
            created: None,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct ListingWire {
    #[serde(flatten)]
    repository: RepositoryWire,
    config: Option<Value>,
    gguf: Option<Value>,
    siblings: Option<Value>,
    tags: Option<Value>,
    #[serde(rename = "createdAt")]
    created: Option<Value>,
}

impl ListingWire {
    pub(super) fn listing(self, format: Format) -> Option<ModelListing> {
        let repository = Repository::parse(self.repository.id()?).ok()?;
        let config = self.config.as_ref().unwrap_or(&Value::Null);
        let gguf = self.gguf.as_ref().unwrap_or(&Value::Null);
        let architecture = config["architectures"]
            .get(0)
            .and_then(Value::as_str)
            .and_then(architecture_label)
            .or_else(|| config["model_type"].as_str().and_then(architecture_label))
            .or_else(|| gguf["architecture"].as_str().and_then(architecture_label));
        let context = context_length(config)
            .or_else(|| context_length(&config["text_config"]))
            .or_else(|| positive(&gguf["context_length"]));
        let license = self
            .tags
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|tag| tag.strip_prefix("license:"))
                    .find_map(license_label)
            });
        let created = super::publication_timestamp(self.created.as_ref());
        let (quantizations, bytes) = match format {
            Format::Gguf => weight_summary(self.siblings.as_ref(), gguf),
            Format::Mlx => (
                MLX_QUANTIZATION
                    .captures(repository.id())
                    .and_then(|captures| captures.get(1))
                    .map(|bits| format!("{}-bit", bits.as_str()))
                    .into_iter()
                    .collect(),
                None,
            ),
        };
        Some(ModelListing {
            repository,
            format,
            architecture,
            quantizations,
            license,
            bytes,
            context,
            created,
        })
    }
}

fn context_length(config: &Value) -> Option<u64> {
    [
        "max_position_embeddings",
        "context_length",
        "max_sequence_length",
        "seq_length",
        "n_ctx",
    ]
    .into_iter()
    .find_map(|key| positive(&config[key]))
}

fn positive(value: &Value) -> Option<u64> {
    value.as_u64().filter(|&number| number > 0)
}

fn architecture_label(value: &str) -> Option<String> {
    (value.len() <= 80
        && !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then(|| value.to_owned())
}

fn weight_summary(siblings: Option<&Value>, gguf: &Value) -> (Vec<String>, Option<u64>) {
    let Some(siblings) = siblings
        .and_then(Value::as_array)
        .filter(|files| files.len() <= MAX_FILES)
    else {
        return (Vec::new(), None);
    };
    let mut quantizations = BTreeSet::new();
    let mut weights = 0;
    let mut single_weight_bytes = None;
    for file in siblings {
        let Some(path) = file["rfilename"]
            .as_str()
            .filter(|path| valid_file_path(path))
        else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.to_ascii_lowercase().starts_with("imatrix") {
            continue;
        }
        weights += 1;
        if split::components(path).is_none() {
            single_weight_bytes =
                positive(&file["size"]).or_else(|| positive(&gguf["totalFileSize"]));
        }
        if let Some(quantization) = QUANTIZATION
            .captures(name)
            .and_then(|captures| captures.get(1))
        {
            quantizations.insert(quantization.as_str().to_ascii_uppercase());
        }
    }
    let bytes = single_weight_bytes.filter(|&size| weights == 1 && size <= MAX_MODEL_BYTES);
    (quantizations.into_iter().collect(), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listing_uses_typed_metadata_and_does_not_report_alternatives_as_one_model() {
        let listing = |wire| {
            serde_json::from_value::<ListingWire>(wire)
                .unwrap()
                .listing(Format::Gguf)
                .unwrap()
        };
        let mut wire = json!({
            "id": "example/model",
            "config": {"architectures": ["FixtureModel"], "text_config": {"max_position_embeddings": 32768}},
            "gguf": {"architecture": "other", "context_length": 2048, "totalFileSize": 12345},
            "siblings": [{"rfilename": "model-Q4_K_M.gguf"}, {"rfilename": "mmproj-F16.gguf"}],
            "tags": ["license:apache-2.0"],
            "createdAt": "2024-02-03T00:00:00.000Z"
        });
        let one = listing(wire.clone());
        assert_eq!(one.architecture.as_deref(), Some("FixtureModel"));
        assert_eq!(one.license.as_deref(), Some("apache-2.0"));
        assert_eq!(one.context, Some(32768));
        assert_eq!(one.bytes, Some(12345));
        assert_eq!(one.quantizations, ["Q4_K_M"]);
        assert!(one.created.is_some());
        wire["siblings"]
            .as_array_mut()
            .unwrap()
            .push(json!({"rfilename": "model-Q8_0-00001-of-00002.gguf"}));
        let multiple = listing(wire);
        assert_eq!(multiple.quantizations, ["Q4_K_M", "Q8_0"]);
        assert_eq!(multiple.bytes, None);
    }
}
