//! Model declarations read from the installation policy.

use std::collections::BTreeSet;
use std::{fmt, io};

use toml_edit::{Array, Item};

use super::{allowed, invalid, string, trim_java};

const SECTION: &str = "ai-runtime";
const MODELS: &str = "model";
const ID_PREFIX: &str = "policy-";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicyModelRuntime {
    Gguf,
    Mlx,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PolicyModel {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) runtime: PolicyModelRuntime,
    pub(crate) source: String,
    pub(crate) digest: Option<[u8; 32]>,
    pub(crate) protected_cache: Option<String>,
}

impl fmt::Debug for PolicyModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PolicyModel")
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

pub(super) fn parse(item: Option<Item>) -> io::Result<Vec<PolicyModel>> {
    let Some(item) = item else {
        return Ok(Vec::new());
    };
    let section = item
        .as_table()
        .ok_or_else(|| invalid("ai-runtime must be a table"))?;
    allowed(section, SECTION, &[MODELS])?;
    let Some(item) = section
        .get(MODELS)
        .filter(|item| !item.as_array().is_some_and(Array::is_empty))
    else {
        return Ok(Vec::new());
    };
    let tables = item
        .as_array_of_tables()
        .ok_or_else(|| invalid("ai-runtime.model must be an array of tables"))?;

    let mut ids = BTreeSet::new();
    let mut models = Vec::with_capacity(tables.len());
    for (index, table) in tables.iter().enumerate() {
        let scope = format!("ai-runtime.model #{}", index + 1);
        allowed(
            table,
            &scope,
            &["name", "runtime", "source", "sha256", "protected-cache"],
        )?;
        let required = |key| {
            string(table, &scope, key)?
                .map(trim_java)
                .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
                .ok_or_else(|| invalid(format!("{scope}.{key} must be non-empty single-line text")))
        };
        let name = required("name")?;
        let runtime = match required("runtime")?.to_ascii_lowercase().as_str() {
            "llama" => PolicyModelRuntime::Gguf,
            "mlx" => PolicyModelRuntime::Mlx,
            _ => return Err(invalid(format!("{scope}.runtime must be llama or mlx"))),
        };
        // Source resolution is platform-specific and happens when the runtime opens the model.
        let source = required("source")?;
        let digest = string(table, &scope, "sha256")?
            .map(|value| parse_digest(value, &scope))
            .transpose()?;
        let protected_cache = string(table, &scope, "protected-cache")?
            .map(|value| {
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(invalid(format!(
                        "{scope}.protected-cache must be non-empty single-line text"
                    )));
                }
                Ok(value.to_owned())
            })
            .transpose()?;
        let id = model_id(name);
        if !valid_policy_model_id(&id) {
            return Err(invalid(format!(
                "{scope}.name gives an invalid model identifier"
            )));
        }
        if !ids.insert(id.clone()) {
            return Err(invalid(format!(
                "{scope}.name gives a duplicate model identifier"
            )));
        }
        models.push(PolicyModel {
            id,
            name: name.to_owned(),
            runtime,
            source: source.to_owned(),
            digest,
            protected_cache,
        });
    }
    Ok(models)
}

fn parse_digest(value: &str, scope: &str) -> io::Result<[u8; 32]> {
    let error = || {
        invalid(format!(
            "{scope}.sha256 must contain exactly 64 hexadecimal digits"
        ))
    };
    let mut digest = [0; 32];
    if value.len() != digest.len() * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error());
    }
    for (index, byte) in digest.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&value[start..start + 2], 16).map_err(|_| error())?;
    }
    Ok(digest)
}

fn model_id(name: &str) -> String {
    // The Java provisioner preserves ASCII letters, digits, dots, underscores and hyphens.
    let lower = name.to_lowercase();
    let slug = lower
        .split(|ch: char| !id_character(ch))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "model" } else { slug };
    format!("{ID_PREFIX}{slug}")
}

fn id_character(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
}

/// Checks the alias syntax shared with Java. Authority requires a current policy declaration.
pub(crate) fn valid_policy_model_id(id: &str) -> bool {
    id.len() <= crate::local_ai::MAX_MODEL_BYTES
        && id.strip_prefix(ID_PREFIX).is_some_and(|slug| {
            !slug.is_empty()
                && !slug.starts_with('-')
                && !slug.ends_with('-')
                && slug.chars().all(id_character)
        })
}

#[cfg(test)]
mod tests {
    use super::{super::parse_with_identity, *};

    const META: &str = "[meta]\nschema-version = 1\n";

    #[test]
    fn ai_runtime_model_policy_keeps_global_declarations_through_scoped_rules() {
        let source = format!(
            "{META}{}",
            r#"
[[rule]]
[rule.ai-runtime]
allow-user-models = false
allow-model-downloads = false
[[rule]]
users = ["reviewer"]
[rule.ai-runtime]
allow-user-models = true
[[ai-runtime.model]]
name = "  ACME Llama Q4  "
runtime = " LLaMA "
source = '/srv/models/model.gguf'
[[ai-runtime.model]]
name = "ACME MLX"
runtime = "mlx"
source = '\\storage\models\mlx-model'
[[ai-runtime.model]]
name = "Remote GGUF"
runtime = "llama"
source = 'https://models.invalid/remote.gguf'
"#
        );
        let read = |name| parse_with_identity(&source, None, || Ok(name), || Ok(&[])).unwrap();
        let viewer = read("viewer");
        let reviewer = read("reviewer");
        assert!(!viewer.ai_user_model_files_allowed());
        assert!(reviewer.ai_user_model_files_allowed());
        assert!(!viewer.ai_model_downloads_allowed());
        let models = viewer.ai_runtime_models();
        assert_eq!(models, reviewer.ai_runtime_models());
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].name, "ACME Llama Q4");
        assert_eq!(models[0].id, "policy-acme-llama-q4");
        assert_eq!(models[0].runtime, PolicyModelRuntime::Gguf);
        assert_eq!(models[0].source, "/srv/models/model.gguf");
        assert_eq!(models[0].digest, None);
        assert_eq!(models[1].runtime, PolicyModelRuntime::Mlx);
        assert_eq!(models[1].source, r"\\storage\models\mlx-model");
        assert_eq!(models[2].source, "https://models.invalid/remote.gguf");
        assert_eq!(models[2].digest, None);
        assert_eq!(models[2].protected_cache, None);
    }

    #[test]
    fn ai_runtime_model_policy_checks_aliases_against_pinned_java_examples() {
        for (name, expected) in [
            ("ACME Llama Q4", "policy-acme-llama-q4"),
            ("model/with:weird*chars", "policy-model-with-weird-chars"),
            ("llama-3.3_70b", "policy-llama-3.3_70b"),
            ("  spaced  ", "policy-spaced"),
            ("***", "policy-model"),
        ] {
            let id = model_id(name);
            assert_eq!(id, expected);
            assert!(valid_policy_model_id(&id));
        }
        for id in [
            "policy-",
            "policy--a",
            "policy-a-",
            "policy-A",
            "policy-../a",
        ] {
            assert!(!valid_policy_model_id(id));
        }
    }

    #[test]
    fn ai_runtime_model_policy_rejects_duplicate_ids_and_invalid_shapes() {
        let model = "[[ai-runtime.model]]\nname='Example model'\nruntime='llama'\nsource='/models/example.gguf'\n";
        let duplicate =
            "[[ai-runtime.model]]\nname='EXAMPLE/model'\nruntime='mlx'\nsource='/models/mlx'\n";
        let invalid_shapes = [
            format!("ai-runtime = 9\n{META}"),
            format!("{META}[ai-runtime]\nmodel = 'not an array'\n"),
            format!("{META}{model}{duplicate}"),
            format!("{META}{}", model.replace("'llama'", "'gguf'")),
            format!("{META}{}", model.replace("'/models/example.gguf'", "22")),
            format!("{META}{}", model.replace("'/models/example.gguf'", "''")),
            format!(
                "{META}{}",
                model.replace("source='/models/example.gguf'\n", "")
            ),
            format!("{META}{model}hash='unchecked'\n"),
            format!("{META}[[rule]]\n[rule.ai-runtime]\nmodel = []\n"),
        ];
        for source in invalid_shapes {
            let error = parse_with_identity(&source, None, || Ok("operator"), || Ok(&[]))
                .err()
                .expect("invalid model declaration");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        let empty = format!("{META}[ai-runtime]\nmodel = []\n");
        let policy = parse_with_identity(&empty, None, || Ok("operator"), || Ok(&[])).unwrap();
        assert!(policy.ai_runtime_models().is_empty());
    }

    #[test]
    fn ai_runtime_model_policy_parses_an_exact_optional_sha256() {
        let model = format!(
            "{META}[[ai-runtime.model]]\nname='Remote GGUF'\nruntime='llama'\nsource='http://models.invalid/example.gguf'\n"
        );
        let digest = "aB".repeat(32);
        let source = format!("{model}sha256='{digest}'\n");
        let policy = parse_with_identity(&source, None, || Ok("operator"), || Ok(&[])).unwrap();
        assert_eq!(policy.ai_runtime_models()[0].digest, Some([0xab; 32]));

        for value in [
            format!("'{}'", &digest[..63]),
            format!("'{digest}0'"),
            format!("' {digest}'"),
            format!("'{}z'", &digest[..63]),
            format!("'{}é'", &digest[..62]),
            "123".into(),
        ] {
            let source = format!("{model}sha256={value}\n");
            let error = parse_with_identity(&source, None, || Ok("operator"), || Ok(&[]))
                .err()
                .expect("invalid digest");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("ai-runtime.model #1.sha256"));
        }
    }

    #[test]
    fn ai_runtime_model_policy_preserves_protected_cache_path() {
        let model = format!(
            "{META}[[ai-runtime.model]]\nname='Remote GGUF'\nruntime='llama'\nsource='https://models.invalid/example.gguf'\n"
        );
        let source = format!("{model}protected-cache='/srv/AI Models/example.gguf'\n");
        let policy = parse_with_identity(&source, None, || Ok("operator"), || Ok(&[])).unwrap();
        assert_eq!(
            policy.ai_runtime_models()[0].protected_cache.as_deref(),
            Some("/srv/AI Models/example.gguf")
        );

        for value in ["123", "''", "'  '", r#""/srv/line\nbreak.gguf""#] {
            let source = format!("{model}protected-cache={value}\n");
            let error = parse_with_identity(&source, None, || Ok("operator"), || Ok(&[]))
                .err()
                .expect("invalid protected cache declaration");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                error
                    .to_string()
                    .contains("ai-runtime.model #1.protected-cache")
            );
        }
    }
}
