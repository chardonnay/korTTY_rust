//! Offline provider and model suggestions for HTTPS profile setup.

use std::fmt;

use super::{ConnectionMode, Profile};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Provider {
    OpenAi,
    Anthropic,
    Gemini,
    Mistral,
    DeepSeek,
    Groq,
    OpenRouter,
    MiniMax,
}

impl Provider {
    pub(super) const ALL: [Self; 8] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::Mistral,
        Self::DeepSeek,
        Self::Groq,
        Self::OpenRouter,
        Self::MiniMax,
    ];

    pub(super) fn mode(self) -> ConnectionMode {
        match self {
            Self::Anthropic => ConnectionMode::Anthropic,
            _ => ConnectionMode::RemoteOpenAi,
        }
    }

    pub(super) fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Mistral => "https://api.mistral.ai/v1",
            Self::DeepSeek => "https://api.deepseek.com/v1",
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::MiniMax => "https://api.minimax.io/v1",
        }
    }

    pub(super) fn matches_endpoint(self, profile: &Profile) -> bool {
        profile.connection_mode == self.mode()
            && profile.remote_protocol().is_some_and(|protocol| {
                crate::local_ai::remote_api_base(&profile.endpoint, protocol)
                    .is_ok_and(|base| base == self.endpoint())
            })
    }

    fn examples(self) -> &'static [&'static str] {
        match self {
            Self::OpenAi => &["gpt-4o-mini", "gpt-4o", "o4-mini"],
            Self::Anthropic => &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"],
            Self::Gemini => &["gemini-2.0-flash", "gemini-1.5-pro"],
            Self::Mistral => &["mistral-large-latest", "mistral-small-latest"],
            Self::DeepSeek => &["deepseek-chat", "deepseek-reasoner"],
            Self::Groq => &["llama-3.3-70b-versatile", "openai/gpt-oss-20b"],
            Self::OpenRouter => &["openai/gpt-4o-mini", "anthropic/claude-3.5-sonnet"],
            Self::MiniMax => &["MiniMax-Text-01"],
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic (Claude)",
            Self::Gemini => "Google Gemini",
            Self::Mistral => "Mistral",
            Self::DeepSeek => "DeepSeek",
            Self::Groq => "Groq",
            Self::OpenRouter => "OpenRouter",
            Self::MiniMax => "MiniMax Global (OpenAI-compatible)",
        })
    }
}

pub(super) fn model_choices(profile: &Profile, discovered: &[String]) -> Vec<String> {
    let mut models = discovered.to_vec();
    let provider = profile.remote_protocol().and_then(|protocol| {
        let base = crate::local_ai::remote_api_base(&profile.endpoint, protocol).ok()?;
        let url = url::Url::parse(&base).ok()?;
        let host = url.host_str()?;
        Provider::ALL.into_iter().find(|provider| {
            provider.mode() == profile.connection_mode
                && provider
                    .endpoint()
                    .strip_prefix("https://")
                    .and_then(|base| base.split('/').next())
                    == Some(host)
        })
    });
    if let Some(provider) = provider {
        models.extend(
            provider
                .examples()
                .iter()
                .filter(|example| !discovered.iter().any(|model| model == **example))
                .map(|example| (*example).to_owned()),
        );
    }
    models
}
