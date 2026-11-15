//! Chat-template controls supported by the reviewed signed mlx-lm runtimes.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::read_json;

pub(crate) const UNSUPPORTED: &str =
    "This MLX model's reviewed chat template does not support the selected reasoning setting.";
const MAX_CONTROL_METADATA: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Controls {
    #[default]
    DefaultOnly,
    GptOssEffort,
    QwenThinking,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(in crate::local_ai) enum TemplateArguments<'a> {
    Effort { reasoning_effort: &'a str },
    Thinking { enable_thinking: bool },
}

impl Controls {
    pub(in crate::local_ai) fn runtime_supported(version: &str, requirements: &str) -> bool {
        // Signed dependency locks of the published 0.31.3 ZIP and Java v2.26.0's 0.32.2 pin.
        // The corresponding Transformers releases both read standalone templates first.
        match version {
            "0.31.3" => requirements.eq_ignore_ascii_case(
                "904ff3afefe2c94391fce24ee91e6c31da9cf16b6a5035236aa650400bbc45e0",
            ),
            "0.32.2" => requirements.eq_ignore_ascii_case(
                "a3ed84516e5bd88b8d1557dcf34494100d681543c409974e910fe5d8abdd53a4",
            ),
            _ => false,
        }
    }

    pub(crate) fn efforts(self) -> &'static [&'static str] {
        match self {
            Self::DefaultOnly => &[],
            Self::GptOssEffort => &["low", "medium", "high"],
            Self::QwenThinking => &["none", "on"],
        }
    }

    pub(crate) fn supports(self, effort: Option<&str>) -> bool {
        effort.is_none_or(|value| self.efforts().contains(&value))
    }

    pub(in crate::local_ai) fn arguments<'a>(
        self,
        effort: &'a str,
    ) -> Result<TemplateArguments<'a>, &'static str> {
        match (self, effort) {
            (Self::GptOssEffort, "low" | "medium" | "high") => Ok(TemplateArguments::Effort {
                reasoning_effort: effort,
            }),
            (Self::QwenThinking, "none" | "on") => Ok(TemplateArguments::Thinking {
                enable_thinking: effort == "on",
            }),
            _ => Err(UNSUPPORTED),
        }
    }

    pub(super) fn read(directory: &Directory) -> io::Result<Self> {
        Self::from_metadata(|name| {
            let mut file = match directory.open_file(name) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            require_single_link(&file)?;
            let stamp = FileStamp::read(&file)?;
            let bytes = read_metadata(&mut file)?;
            stamp.require_unchanged(&file)?;
            stamp.require_unchanged(&directory.open_file(name)?)?;
            Ok(Some(bytes))
        })
    }

    pub(super) fn from_files(files: &mut BTreeMap<String, File>) -> io::Result<Self> {
        Self::from_metadata(|name| files.get_mut(name).map(read_metadata).transpose())
    }

    fn from_metadata(
        mut read: impl FnMut(&str) -> io::Result<Option<Vec<u8>>>,
    ) -> io::Result<Self> {
        let Some(config) = read("config.json")? else {
            return Ok(Self::DefaultOnly);
        };
        let config = read_json(config.as_slice())?;
        let model_type = config.get("model_type").and_then(serde_json::Value::as_str);
        if !matches!(model_type, Some("gpt_oss" | "qwen3")) {
            return Ok(Self::DefaultOnly);
        }
        let Some(tokenizer) = read("tokenizer_config.json")? else {
            return Ok(Self::DefaultOnly);
        };
        let tokenizer = read_json(tokenizer.as_slice())?;
        if !tokenizer.is_object() || tokenizer.get("chat_template_type").is_some() {
            return Ok(Self::DefaultOnly);
        }
        if let Some(template) = read("chat_template.jinja")? {
            return Ok(Self::known_template(model_type, &template));
        }
        Ok(tokenizer
            .get("chat_template")
            .and_then(serde_json::Value::as_str)
            .map_or(Self::DefaultOnly, |template| {
                Self::known_template(model_type, template.as_bytes())
            }))
    }

    fn known_template(model_type: Option<&str>, template: &[u8]) -> Self {
        // Use exact reviewed templates. A model name or an occurrence of a Jinja variable
        // in a comment does not establish that the effective template reads the setting.
        Self::reviewed_digest(model_type, &format!("{:x}", Sha256::digest(template)))
    }

    fn reviewed_digest(model_type: Option<&str>, digest: &str) -> Self {
        match (model_type, digest) {
            // openai/gpt-oss-20b @ 6cee5e81, also shipped by mlx-community as MXFP4-Q4 and Q8.
            (
                Some("gpt_oss"),
                "a4c9919cbbd4acdd51ccffe22da049264b1b73e59055fa58811a99efbd7c8146",
            ) => Self::GptOssEffort,
            // Qwen/Qwen3-8B-MLX-4bit @ 383413e9, mlx-community/Qwen3-8B-4bit @ 545dc425.
            (
                Some("qwen3"),
                "57f1fd00f0013a2be96aa79b857391f27e23df5b5f847072b524c897e24d0361"
                | "87a2728cb8dc9fe424d624542f6060ec05a1d285ebbec578bb078900e33396b5",
            ) => Self::QwenThinking,
            _ => Self::DefaultOnly,
        }
    }
}

fn read_metadata(file: &mut File) -> io::Result<Vec<u8>> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(MAX_CONTROL_METADATA + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONTROL_METADATA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MLX reasoning metadata exceeds the inspection limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mlx_controls_require_the_reviewed_template_and_architecture() {
        let known = "57f1fd00f0013a2be96aa79b857391f27e23df5b5f847072b524c897e24d0361";
        assert_eq!(
            Controls::reviewed_digest(Some("qwen3"), known),
            Controls::QwenThinking
        );
        assert_eq!(
            Controls::reviewed_digest(Some("llama"), known),
            Controls::DefaultOnly
        );
        assert_eq!(
            Controls::known_template(Some("qwen3"), b"{# enable_thinking #}{{ messages }}"),
            Controls::DefaultOnly,
        );
        assert!(Controls::QwenThinking.arguments("low").is_err());
        assert!(Controls::GptOssEffort.arguments("none").is_err());
        let released = "904ff3afefe2c94391fce24ee91e6c31da9cf16b6a5035236aa650400bbc45e0";
        assert!(Controls::runtime_supported("0.31.3", released));
        assert!(!Controls::runtime_supported("0.32.2", released));
    }
}
