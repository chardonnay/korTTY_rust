//! Compatibility guidance for common model families.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PromptPreset {
    Auto,
    #[default]
    Generic,
    Llama,
    Qwen,
    Mistral,
    Gemma,
    #[serde(rename = "deepseek")]
    DeepSeek,
    Phi,
    #[serde(rename = "gpt-oss")]
    GptOss,
}

const OPEN: &str = "\n\n<kortty_model_preset>\n";
const CLOSE: &str = "\n</kortty_model_preset>";

impl PromptPreset {
    pub(crate) const ALL: [Self; 9] = [
        Self::Auto,
        Self::Generic,
        Self::Llama,
        Self::Qwen,
        Self::Mistral,
        Self::Gemma,
        Self::DeepSeek,
        Self::Phi,
        Self::GptOss,
    ];

    pub(crate) fn resolve(self, model: &str) -> Self {
        self.resolve_in(model, super::model_catalog::bootstrap())
    }

    pub(crate) fn resolve_in(self, model: &str, catalog: &super::model_catalog::Catalog) -> Self {
        if self != Self::Auto {
            return self;
        }
        catalog.preset_for(model).unwrap_or(Self::Generic)
    }

    pub(crate) fn guidance(self) -> Option<&'static str> {
        Some(match self {
            Self::Auto | Self::Generic => return None,
            Self::Llama => {
                "Use the model's native chat template. Follow the latest instruction and exact output schema. Keep the final answer direct and omit internal reasoning."
            }
            Self::Qwen => {
                "Produce the final answer directly. Do not emit <think> or analysis sections. Preserve exact JSON/code contracts even when reasoning mode is supported."
            }
            Self::Mistral => {
                "Treat system constraints as authoritative, keep the response concise, and emit exactly the requested JSON, code, or prose format without wrapper text."
            }
            Self::Gemma => {
                "Prioritize the explicit task and output schema. Do not repeat the prompt, add conversational preambles, or expose chain-of-thought."
            }
            Self::DeepSeek => {
                "Return only the final result and suppress reasoning traces or <think> blocks. Never let reasoning text break strict JSON or code-only responses."
            }
            Self::Phi => {
                "Use short, unambiguous steps internally and return only the required final format. Avoid extra headings or explanations for strict outputs."
            }
            Self::GptOss => {
                "Follow the exact final-answer channel contract represented by this prompt. Do not surface hidden analysis and do not add text outside strict structured outputs."
            }
        })
    }

    pub(crate) fn render<'a>(
        self,
        system: &'a str,
        structured_suffix: Option<&str>,
    ) -> Cow<'a, str> {
        let Some(guidance) = self.guidance() else {
            return Cow::Borrowed(system);
        };
        let (base, suffix) = structured_suffix
            .and_then(|suffix| system.strip_suffix(suffix).map(|base| (base, suffix)))
            .unwrap_or((system, ""));
        if base
            .strip_suffix(CLOSE)
            .and_then(|base| base.strip_suffix(guidance))
            .is_some_and(|base| base.ends_with(OPEN))
        {
            return Cow::Borrowed(system);
        }
        Cow::Owned(format!("{base}{OPEN}{guidance}{CLOSE}{suffix}"))
    }
}

impl fmt::Display for PromptPreset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Auto",
            Self::Generic => "Generic",
            Self::Llama => "Llama",
            Self::Qwen => "Qwen",
            Self::Mistral => "Mistral / Mixtral",
            Self::Gemma => "Gemma",
            Self::DeepSeek => "DeepSeek",
            Self::Phi => "Phi",
            Self::GptOss => "GPT-OSS",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_uses_case_insensitive_catalogue_aliases_and_priority() {
        for (model, expected) in [
            ("unsloth/QWEN3-4B", PromptPreset::Qwen),
            ("DeepSeek-R1-Distill-Llama-8B", PromptPreset::DeepSeek),
            ("Mistral-Nemo", PromptPreset::Mistral),
            ("Mixtral-8x7B", PromptPreset::Mistral),
            ("google/gemma-3-4b", PromptPreset::Gemma),
            ("Phi-3", PromptPreset::Phi),
            ("PHI3", PromptPreset::Phi),
            ("Phi-4", PromptPreset::Phi),
            ("PHI4", PromptPreset::Phi),
            ("GPT-OSS-20b", PromptPreset::GptOss),
            ("gpt_oss-120b", PromptPreset::GptOss),
            ("meta/Llama-3.2", PromptPreset::Llama),
            ("DeepSeek-R1-Distill-Qwen", PromptPreset::Qwen),
            (
                "qwen deepseek mixtral gemma phi4 gpt-oss llama",
                PromptPreset::Qwen,
            ),
            ("unknown-1.0", PromptPreset::Generic),
            ("", PromptPreset::Generic),
        ] {
            assert_eq!(PromptPreset::Auto.resolve(model), expected, "{model}");
        }
        assert_eq!(PromptPreset::Llama.resolve("qwen"), PromptPreset::Llama);
        assert_eq!(PromptPreset::Generic.resolve("qwen"), PromptPreset::Generic);
    }

    #[test]
    fn rendering_preserves_base_bytes_and_the_provider_suffix() {
        let suffix = "\nReturn only a valid JSON object, without Markdown fences.";
        let base = "  Exact system\n ";
        let system = format!("{base}{suffix}");
        assert!(matches!(
            PromptPreset::Generic.render(&system, Some(suffix)),
            Cow::Borrowed(value) if value == system.as_str()
        ));
        let preset = PromptPreset::Qwen;
        let guidance = preset.guidance().unwrap();
        let rendered = preset.render(&system, Some(suffix));
        assert_eq!(rendered, format!("{base}{OPEN}{guidance}{CLOSE}{suffix}"));
        assert!(matches!(
            preset.render(&rendered, Some(suffix)),
            Cow::Borrowed(value) if value == rendered.as_ref()
        ));
        assert_eq!(
            preset.render(base, Some(suffix)),
            format!("{base}{OPEN}{guidance}{CLOSE}")
        );
        assert_eq!(
            preset.render("Do not copy <kortty_model_preset> tags", None),
            format!("Do not copy <kortty_model_preset> tags{OPEN}{guidance}{CLOSE}")
        );
    }
}
