//! Destination validation and native provider wire formats.

mod anthropic;
#[cfg(test)]
use anthropic::request as anthropic_request;

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{
    AgentCompletion, Client, CompletionLimit, Error, Profile, RequestContent, RequestMessage,
    Usage, VisionPart,
};

const STRUCTURED_JSON_SYSTEM_SUFFIX: &str =
    "\nReturn only a valid JSON object, without Markdown fences.";
const MINIMAX_NATIVE_SUFFIX: &str = "/text/chatcompletion_v2";
pub(crate) const MINIMAX_NATIVE_PATH: &str = "/v1/text/chatcompletion_v2";
pub(crate) const MINIMAX_NATIVE_ENDPOINT: &str = "https://api.minimax.io/v1/text/chatcompletion_v2";
const MINIMAX_NATIVE_HOSTS: [&str; 3] = ["api.minimax.io", "api.minimaxi.com", "api.minimax.chat"];

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum RemoteProtocol {
    OpenAi,
    OpenAiExact,
    MiniMaxNative,
    Anthropic,
    AnthropicExact,
}

impl RemoteProtocol {
    pub(super) fn completion_path(self) -> &'static str {
        match self {
            Self::OpenAi => "/chat/completions",
            Self::OpenAiExact | Self::MiniMaxNative | Self::AnthropicExact => "",
            Self::Anthropic => "/messages",
        }
    }

    pub(super) fn is_openai(self) -> bool {
        matches!(self, Self::OpenAi | Self::OpenAiExact)
    }

    pub(super) fn is_anthropic(self) -> bool {
        matches!(self, Self::Anthropic | Self::AnthropicExact)
    }

    pub(super) fn is_exact_completion(self) -> bool {
        matches!(self, Self::OpenAiExact | Self::AnthropicExact)
    }
}

impl Profile {
    pub(crate) fn structured_system_suffix(&self) -> Option<&'static str> {
        (self.uses_lm_studio_protocol() || self.uses_anthropic_protocol())
            .then_some(STRUCTURED_JSON_SYSTEM_SUFFIX)
    }
}

pub(crate) fn is_messages_path(host: Option<&str>, path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("/v1/messages")
        || host.is_some_and(|host| host.ends_with(".anthropic.com"))
            && lower.contains("/v1/messages")
}

pub(crate) fn remote_api_base(endpoint: &str, protocol: RemoteProtocol) -> Result<String, String> {
    let endpoint = endpoint.trim();
    let invalid =
        || "Enter an HTTPS API URL without credentials, a query, or a fragment.".to_owned();
    if endpoint.len() > 2048
        || !(endpoint.starts_with("https://")
            || protocol == RemoteProtocol::AnthropicExact
                && super::lm_studio_endpoint::strip_ascii_prefix(endpoint, "https://").is_some())
        || endpoint.chars().any(|character| {
            character.is_whitespace() || character.is_control() || character == '\\'
        })
        || endpoint.split_once("://").is_some_and(|(_, rest)| {
            rest.split('/')
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    {
        return Err(invalid());
    }
    let url = url::Url::parse(endpoint).map_err(|_| invalid())?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !url.path().bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'~')
        })
    {
        return Err(invalid());
    }
    if protocol.is_exact_completion() {
        let (authority, raw_path) =
            super::lm_studio_endpoint::strip_ascii_prefix(endpoint, "https://")
                .and_then(|rest| rest.split_once('/'))
                .ok_or_else(invalid)?;
        let path = raw_path.strip_suffix('/').unwrap_or(raw_path);
        let matches_protocol = match protocol {
            RemoteProtocol::AnthropicExact => is_messages_path(url.host_str(), url.path()),
            _ => path.to_ascii_lowercase().ends_with("/chat/completions"),
        };
        if protocol.is_anthropic() && (authority.contains('%') || authority.ends_with(':'))
            || url.path().strip_prefix('/') != Some(raw_path)
            || !matches_protocol
            || path
                .split('/')
                .any(|segment| matches!(segment, "" | "." | ".."))
        {
            return Err(if protocol.is_anthropic() {
                "Enter a complete Anthropic Messages HTTPS URL with an unambiguous path."
            } else {
                "Enter a complete HTTPS /chat/completions URL with an unambiguous path."
            }
            .into());
        }
        return Ok(format!("{}/{raw_path}", url.origin().ascii_serialization()));
    }
    let path = url.path().trim_end_matches('/');
    if protocol == RemoteProtocol::MiniMaxNative {
        let (authority, raw_path) = endpoint
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/'))
            .ok_or_else(invalid)?;
        let raw_host = authority.strip_suffix(":443").unwrap_or(authority);
        if path != MINIMAX_NATIVE_PATH
            || Some(raw_path.trim_end_matches('/')) != MINIMAX_NATIVE_PATH.strip_prefix('/')
            || url.port().is_some()
            || !url.host_str().is_some_and(|host| {
                MINIMAX_NATIVE_HOSTS.contains(&host) && raw_host.eq_ignore_ascii_case(host)
            })
        {
            return Err(invalid());
        }
        return Ok(format!("{}{path}", url.origin().ascii_serialization()));
    }
    let path = path
        .strip_suffix(protocol.completion_path())
        .unwrap_or(path);
    let path = if path.is_empty() { "/v1" } else { path };
    Ok(format!("{}{path}", url.origin().ascii_serialization()))
}

pub(crate) fn remote_destination(
    endpoint: &str,
    protocol: RemoteProtocol,
) -> Result<String, String> {
    remote_api_base(endpoint, protocol).map(|base| {
        let path = protocol.completion_path();
        format!("{base}{path}")
    })
}

pub(super) fn remote_model_discovery_base(base: &str, protocol: RemoteProtocol) -> &str {
    if protocol == RemoteProtocol::MiniMaxNative {
        base.strip_suffix(MINIMAX_NATIVE_SUFFIX).unwrap_or(base)
    } else {
        base
    }
}

/// Interpret provider failures without exposing error body text or credentials to the caller.
pub(super) fn openai_provider_error(
    error: Option<&Value>,
    base_resp: Option<&Value>,
    has_choices: bool,
) -> Option<Error> {
    if error.is_some_and(|error| !error.is_null()) {
        return Some(Error::Invalid("the AI provider reported an error"));
    }
    if has_choices {
        return None;
    }
    let code = base_resp?.get("status_code")?;
    if code.is_null() || code.as_str().is_some_and(|value| value.trim().is_empty()) {
        return None;
    }
    let reason = match provider_number(code) {
        Some(0.0) => return None,
        Some(1004.0) => "the MiniMax provider rejected authentication",
        Some(2013.0) => "the MiniMax provider rejected the request parameters",
        _ => "the MiniMax provider reported an error",
    };
    Some(Error::Invalid(reason))
}

fn provider_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|code| code.is_finite()),
        _ => None,
    }
}

pub(super) fn thinking_rejection_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase().replace(['\'', '"', '`'], "");
    [
        "unsupported parameter: thinking",
        "unsupported parameter thinking",
        "unsupported field: thinking",
        "unsupported field thinking",
        "unknown parameter: thinking",
        "unknown parameter thinking",
        "unknown field: thinking",
        "unknown field thinking",
        "unrecognized request argument supplied: thinking",
        "thinking is not supported",
        "thinking not supported",
        "thinking is unsupported",
        "does not support thinking",
    ]
    .iter()
    .any(|pattern| {
        message.match_indices(pattern).any(|(start, matched)| {
            let boundary = |byte: Option<&u8>| {
                byte.is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            };
            boundary(
                start
                    .checked_sub(1)
                    .and_then(|index| message.as_bytes().get(index)),
            ) && boundary(message.as_bytes().get(start + matched.len()))
        })
    })
}

/// Only a pre-generation JSON rejection can be retried inside a single usage attempt.
pub(super) fn minimax_thinking_rejected_envelope(bytes: &[u8]) -> bool {
    if bytes.len() as u64 > super::MAX_ERROR_RESPONSE_BYTES {
        return false;
    }
    let Ok(response) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let Some(base) = response.get("base_resp") else {
        return false;
    };
    base.get("status_code").and_then(provider_number) == Some(2013.0)
        && base
            .get("status_msg")
            .and_then(Value::as_str)
            .is_some_and(thinking_rejection_message)
        && response
            .get("choices")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
        && response.get("error").is_none_or(Value::is_null)
        && response.get("usage").is_none_or(|usage| {
            usage.is_null() || usage.as_object().is_some_and(serde_json::Map::is_empty)
        })
}

pub(super) fn anthropic_stop(reason: &str, require_complete: bool) -> Result<(), Error> {
    match reason {
        "end_turn" | "stop_sequence" => Ok(()),
        "max_tokens" if !require_complete => Ok(()),
        "max_tokens" => Err(Error::Invalid("model output reached its token limit")),
        _ => Err(Error::Invalid("unexpected Anthropic completion reason")),
    }
}

pub(super) fn anthropic_response(
    bytes: &[u8],
    require_complete: bool,
    usage: &mut Usage,
) -> Result<(String, String), Error> {
    anthropic_response_with_stop(bytes, require_complete, usage, None)
}

pub(super) fn anthropic_response_with_stop(
    bytes: &[u8],
    require_complete: bool,
    usage: &mut Usage,
    metadata: Option<&mut AgentCompletion>,
) -> Result<(String, String), Error> {
    let body: Value = serde_json::from_slice(bytes)
        .map_err(|_| Error::Invalid("invalid Anthropic JSON response"))?;
    usage.observe_anthropic(&body);
    let reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid("missing Anthropic completion reason"))?;
    let agent_context_limit = metadata.is_some_and(|metadata| {
        metadata.stop = match reason {
            "max_tokens" => Some(CompletionLimit::Output),
            "model_context_window_exceeded" => Some(CompletionLimit::Context),
            _ => None,
        };
        metadata.stop == Some(CompletionLimit::Context)
    });
    if require_complete || !agent_context_limit {
        anthropic_stop(reason, require_complete)?;
    }
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or(Error::Invalid("missing Anthropic response content"))?;
    let mut text = String::new();
    let mut reasoning = String::new();
    for block in content {
        let (target, key) = match block.get("type").and_then(Value::as_str) {
            Some("text") => (&mut text, "text"),
            Some("thinking") => (&mut reasoning, "thinking"),
            Some("redacted_thinking") => continue,
            _ => return Err(Error::Invalid("unexpected Anthropic response content")),
        };
        let part = block
            .get(key)
            .and_then(Value::as_str)
            .ok_or(Error::Invalid("invalid Anthropic response content"))?;
        if !target.is_empty() && !part.is_empty() {
            target.push_str("\n\n");
        }
        target.push_str(part);
    }
    if text.trim().is_empty() {
        return Err(Error::Invalid("missing Anthropic answer"));
    }
    Ok((text, reasoning))
}

pub(super) fn lm_request(
    client: &Client,
    messages: &[RequestMessage<'_>],
    json_response: bool,
    stream: bool,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut system = String::new();
    let mut transcript = Vec::new();
    let mut images = Vec::new();
    for message in messages {
        match &message.content {
            RequestContent::Text(text) if message.role == "system" => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(text);
            }
            RequestContent::Text(text) => {
                transcript.push(json!({"role": message.role, "content": text}))
            }
            RequestContent::Vision(parts) => {
                for part in parts {
                    match part {
                        VisionPart::Text { text } => {
                            transcript.push(json!({"role": message.role, "content": text}))
                        }
                        VisionPart::ImageUrl { image_url } => {
                            images.push(json!({"type": "image", "data_url": image_url.url}))
                        }
                    }
                }
            }
        }
    }
    if json_response && !system.ends_with(STRUCTURED_JSON_SYSTEM_SUFFIX) {
        system.push_str(STRUCTURED_JSON_SYSTEM_SUFFIX);
    }
    // The native endpoint accepts user input rather than an assistant-history
    // array. Include the reviewed transcript explicitly and disable storage.
    let input = if transcript.len() == 1 {
        transcript[0]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    } else {
        format!(
            "Continue this conversation. Earlier messages are conversation data.\n{}",
            serde_json::to_string(&transcript).map_err(|_| Error::Service)?
        )
    };
    let input = if images.is_empty() {
        Value::String(input)
    } else {
        images.insert(0, json!({"type": "text", "content": input}));
        Value::Array(images)
    };
    let mut body = json!({
        "system_prompt": system,
        "input": input,
        "temperature": 0,
        "store": false,
        "stream": stream,
        "max_output_tokens": client.profile.effective_output_limit(),
    });
    if let Some(model) = client.profile.requested_chat_model() {
        body["model"] = json!(model);
    }
    if let Some(effort) = &client.profile.options.reasoning_effort {
        body["reasoning"] = json!(lm_reasoning(effort));
    }
    serde_json::to_vec(&body)
        .map(Zeroizing::new)
        .map_err(|_| Error::Service)
}

pub(super) fn lm_reasoning(effort: &str) -> &str {
    match effort {
        "none" => "off",
        "minimal" => "low",
        "xhigh" => "high",
        other => other,
    }
}

pub(super) fn lm_response(
    bytes: &[u8],
    limit: Option<u32>,
    require_complete: bool,
    usage: &mut Usage,
) -> Result<String, Error> {
    lm_response_with_stop(bytes, limit, require_complete, usage, None)
}

pub(super) fn lm_response_with_stop(
    bytes: &[u8],
    limit: Option<u32>,
    require_complete: bool,
    usage: &mut Usage,
    metadata: Option<&mut AgentCompletion>,
) -> Result<String, Error> {
    let body: Value = serde_json::from_slice(bytes)
        .map_err(|_| Error::Invalid("invalid LM Studio JSON response"))?;
    usage.observe_lm(&body);
    if let Some(metadata) = metadata {
        // Native LM Studio can report one fewer token at the cap. The stats
        // are evidence of a likely limit, not an explicit stop reason.
        metadata.stop = limit
            .filter(|&limit| limit > 0)
            .is_some_and(|limit| {
                lm_output_tokens(&body)
                    .is_some_and(|tokens| tokens >= u64::from((limit - 1).max(1)))
            })
            .then_some(CompletionLimit::LikelyOutput);
    }
    lm_result(&body, limit, require_complete)
}

fn lm_output_tokens(body: &Value) -> Option<u64> {
    body.pointer("/stats/total_output_tokens")
        .and_then(Value::as_u64)
}

pub(super) fn lm_result(
    body: &Value,
    limit: Option<u32>,
    require_complete: bool,
) -> Result<String, Error> {
    if require_complete
        && limit.is_some_and(|limit| {
            lm_output_tokens(body)
                .is_some_and(|tokens| tokens >= u64::from(limit.saturating_sub(1)))
        })
    {
        return Err(Error::Invalid("model output reached its token limit"));
    }
    let items = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or(Error::Invalid("missing LM Studio output"))?;
    let mut text = String::new();
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("message") {
            let part = item
                .get("content")
                .and_then(Value::as_str)
                .ok_or(Error::Invalid("invalid LM Studio message"))?;
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(part);
        }
    }
    if text.is_empty() {
        return Err(Error::Invalid("missing LM Studio answer"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::{Profile, ProfileOptions, Target, chat_messages};

    #[test]
    fn remote_destinations_are_https_and_never_accept_embedded_credentials() {
        assert_eq!(
            remote_destination(
                "https://api.example.test/gateway/v1/",
                RemoteProtocol::OpenAi
            )
            .unwrap(),
            "https://api.example.test/gateway/v1/chat/completions"
        );
        assert_eq!(
            remote_destination(
                "https://api.anthropic.com/v1/messages",
                RemoteProtocol::Anthropic
            )
            .unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        for invalid in [
            "http://api.example.test",
            "https://user:pass@api.example.test/v1",
            "https://@api.example.test/v1",
            "https://api.example.test/v1?q=1",
            "https://api.example.test/v1#fragment",
            "https://api.example.test:0",
            "https://api.example.test/a%2Fb",
        ] {
            assert!(
                remote_destination(invalid, RemoteProtocol::OpenAi).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn minimax_native_destination_preserves_the_explicit_host_and_path() {
        for host in MINIMAX_NATIVE_HOSTS {
            let endpoint = format!("https://{host}{MINIMAX_NATIVE_PATH}");
            for supplied in [endpoint.clone(), format!("{endpoint}/")] {
                let base = remote_api_base(&supplied, RemoteProtocol::MiniMaxNative).unwrap();
                assert_eq!(base, endpoint);
                assert_eq!(
                    remote_destination(&supplied, RemoteProtocol::MiniMaxNative).unwrap(),
                    endpoint
                );
                assert_eq!(
                    remote_model_discovery_base(&base, RemoteProtocol::MiniMaxNative),
                    format!("https://{host}/v1")
                );
            }
        }
        let global = "https://api.minimax.io/v1";
        assert_eq!(
            remote_destination(global, RemoteProtocol::OpenAi).unwrap(),
            "https://api.minimax.io/v1/chat/completions"
        );
        assert_eq!(
            remote_model_discovery_base(global, RemoteProtocol::OpenAi),
            global
        );
        assert_eq!(
            remote_destination(
                "https://Api.MiniMax.IO:443/v1/text/chatcompletion_v2",
                RemoteProtocol::MiniMaxNative,
            )
            .unwrap(),
            "https://api.minimax.io/v1/text/chatcompletion_v2"
        );
        for rejected in [
            "https://api.minimax.io.example.test/v1/text/chatcompletion_v2",
            "https://api.minimax.io@evil.example/v1/text/chatcompletion_v2",
            "https://api.minimax.io:8443/v1/text/chatcompletion_v2",
            "https://api.minimax.io/v1/text/chatcompletion_v2?key=example",
            "https://api.minimax.io/v1/text/chatcompletion_v2#token",
            "https://api.minimax.io/gateway/v1/text/chatcompletion_v2",
            "https://api.minimax.io/v1/text/chatcompletion_v2/chat/completions",
            "https://api.minimax.io/v1/../v1/text/chatcompletion_v2",
            "https://api.minimax.io/v1/%2e/text/chatcompletion_v2",
            "https://%61pi.minimax.io/v1/text/chatcompletion_v2",
        ] {
            assert!(
                remote_destination(rejected, RemoteProtocol::MiniMaxNative).is_err(),
                "{rejected}"
            );
        }
        assert!(remote_destination(global, RemoteProtocol::MiniMaxNative).is_err());
    }

    #[test]
    fn generic_routes_keep_native_shaped_paths_as_bases() {
        for host in ["api.minimax.io", "gateway.example.test"] {
            let base = format!("https://{host}{MINIMAX_NATIVE_PATH}");
            let discovery = remote_api_base(&base, RemoteProtocol::OpenAi).unwrap();
            assert_eq!(
                remote_model_discovery_base(&discovery, RemoteProtocol::OpenAi),
                base
            );
            assert_eq!(
                remote_destination(&base, RemoteProtocol::OpenAi).unwrap(),
                format!("{base}/chat/completions")
            );
            assert_eq!(
                remote_destination(&base, RemoteProtocol::Anthropic).unwrap(),
                format!("{base}/messages")
            );
        }
    }

    #[test]
    fn openai_provider_errors_cannot_turn_into_visible_provider_text() {
        for code in [json!(0), json!(" 0.0 ")] {
            let base = json!({"status_code": code});
            assert!(openai_provider_error(Some(&Value::Null), Some(&base), false).is_none());
        }
        for code in [json!(1004), json!("1004")] {
            let base = json!({"status_code": code, "status_msg": "echoed-private-token"});
            assert!(matches!(
                openai_provider_error(None, Some(&base), false),
                Some(Error::Invalid(
                    "the MiniMax provider rejected authentication"
                ))
            ));
            assert!(openai_provider_error(None, Some(&base), true).is_none());
        }
        let base = json!({"status_code": 2013});
        assert!(matches!(
            openai_provider_error(None, Some(&base), false),
            Some(Error::Invalid(
                "the MiniMax provider rejected the request parameters"
            ))
        ));
        let base = json!({"status_code": "unknown error"});
        assert!(matches!(
            openai_provider_error(None, Some(&base), false),
            Some(Error::Invalid("the MiniMax provider reported an error"))
        ));
        let explicit = json!({"message": "echoed-private-token"});
        assert!(matches!(
            openai_provider_error(Some(&explicit), None, true),
            Some(Error::Invalid("the AI provider reported an error"))
        ));
    }

    #[test]
    fn anthropic_requests_separate_system_and_vision_and_reject_tool_results() {
        let profile = Profile::remote(
            "https://api.anthropic.com",
            "fixture",
            RemoteProtocol::Anthropic,
            ProfileOptions::default(),
        )
        .unwrap();
        let client = Client::new(profile);
        let messages = chat_messages(
            "system rules",
            &[("earlier question", "earlier answer")],
            "new question",
        );
        let request = anthropic_request(&client, &messages, true, true).unwrap();
        let body: Value = serde_json::from_slice(&request).unwrap();
        let reviewed = format!("system rules{STRUCTURED_JSON_SYSTEM_SUFFIX}");
        assert_eq!(body["system"].as_str(), Some(reviewed.as_str()));
        let messages = chat_messages(&reviewed, &[], "new question");
        let request = anthropic_request(&client, &messages, true, true).unwrap();
        let reviewed_body: Value = serde_json::from_slice(&request).unwrap();
        assert_eq!(reviewed_body["system"], body["system"]);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][2]["content"], "new question");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert!(body.get("tools").is_none());
        let response = br#"{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"review"},{"type":"text","text":"answer"}]}"#;
        assert_eq!(
            anthropic_response(response, true, &mut Usage::default()).unwrap(),
            ("answer".into(), "review".into())
        );
        let tool = br#"{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"external"}]}"#;
        assert!(anthropic_response(tool, true, &mut Usage::default()).is_err());
    }

    #[test]
    fn official_openai_payload_sets_modern_cap_without_changing_compatible_payloads() {
        let messages = chat_messages("system", &[], "question");
        for (endpoint, modern) in [
            ("https://api.openai.com/v1", true),
            ("https://fixture.example.test/v1", false),
        ] {
            let client = Client::new(
                Profile::remote(
                    endpoint,
                    "fixture",
                    RemoteProtocol::OpenAi,
                    ProfileOptions::default(),
                )
                .unwrap(),
            );
            let body: Value =
                serde_json::from_slice(&client.openai_request(&messages, false, true).unwrap())
                    .unwrap();
            if modern {
                assert_eq!(body["max_completion_tokens"], 4096);
                assert_eq!(body["store"], false);
                assert!(body.get("temperature").is_none());
            } else {
                assert_eq!(body["max_tokens"], 4096);
                assert_eq!(body["temperature"], 0);
            }
        }
    }

    #[test]
    fn server_default_model_preserves_the_https_route_without_model_specific_parameters() {
        let endpoint = "https://api.minimax.io/gateway/v1/chat/completions";
        let options = ProfileOptions::default();
        let profile =
            Profile::openai_with_server_default(endpoint, RemoteProtocol::OpenAi, options.clone())
                .unwrap();
        let Target::Remote(base, protocol) = &profile.target else {
            panic!("expected an OpenAI-compatible HTTPS target")
        };
        assert_eq!(format!("{base}{}", protocol.completion_path()), endpoint);
        let client = Client::new(profile);
        let messages = chat_messages("system", &[], "question");
        for streaming in [false, true] {
            let request = client.openai_request(&messages, false, streaming).unwrap();
            let body: Value = serde_json::from_slice(&request).unwrap();
            assert!(body.get("model").is_none());
            assert!(body.get("thinking").is_none());
            assert!(body.get("reasoning_effort").is_none());
            assert_eq!(body["stream"], streaming);
        }
        assert!(Profile::remote(endpoint, "", RemoteProtocol::OpenAi, options.clone()).is_err());
        assert!(
            Profile::openai_with_server_default(
                "http://api.minimax.io/v1",
                RemoteProtocol::OpenAi,
                options
            )
            .is_err()
        );
        for effort in ["none", "on", "high"] {
            let options = ProfileOptions {
                reasoning_effort: Some(effort.to_owned()),
                ..ProfileOptions::default()
            };
            assert!(
                Profile::openai_with_server_default(
                    endpoint,
                    RemoteProtocol::OpenAi,
                    options.clone()
                )
                .is_err()
            );
            assert!(
                Profile::lm_studio_with_server_default("127.0.0.1:1234", options.clone(), true)
                    .is_err()
            );
            assert!(Profile::configured_with_server_default("127.0.0.1:1234", options).is_err());
        }
    }

    #[test]
    fn openai_exact_https_keeps_the_completion_path_and_explicit_model_semantics() {
        let protocol = RemoteProtocol::OpenAiExact;
        let endpoint = "https://gateway.example/tenant/V1/Chat/Completions/";
        for valid in [
            endpoint,
            "https://gateway.example/tenant/v1/chat/completions/",
            "https://gateway.example/tenant/v1/CHAT/COMPLETIONS",
        ] {
            assert_eq!(remote_destination(valid, protocol).unwrap(), valid);
        }
        for invalid in [
            "http://gateway.example/v1/chat/completions/",
            "https://gateway.example/tenant/v1",
            "https://gateway.example/v1/responses",
            "https://gateway.example/tenant//chat/completions/",
            "https://gateway.example/v1/chat/completions//",
            "https://gateway.example/tenant/../v1/chat/completions/",
            "https://gateway.example/tenant/%2e/v1/chat/completions/",
            "https://gateway.example/tenant/v1/%63hat/completions/",
            "https://gateway.example/v1/chat/completions/?credential=unsafe",
            "https://gateway.example/v1/chat/completions/#other",
            "https://user@gateway.example/v1/chat/completions/",
        ] {
            assert!(remote_destination(invalid, protocol).is_err(), "{invalid}");
        }
        assert!(crate::local_ai::discover_remote(endpoint, protocol, None).is_err());
        for model in [Some("manual/model"), None] {
            let options = ProfileOptions::default();
            let profile = match model {
                Some(model) => Profile::remote(endpoint, model, protocol, options).unwrap(),
                None => Profile::openai_with_server_default(endpoint, protocol, options).unwrap(),
            };
            let Target::Remote(base, route) = &profile.target else {
                panic!("expected an HTTPS completion endpoint")
            };
            assert_eq!(format!("{base}{}", route.completion_path()), endpoint);
            assert_eq!(profile.uses_server_default_model(), model.is_none());
            let client = Client::new(profile);
            let messages = chat_messages("system", &[], "question");
            for streaming in [false, true] {
                let request = client.openai_request(&messages, false, streaming).unwrap();
                let body: Value = serde_json::from_slice(&request).unwrap();
                assert_eq!(body.get("model").is_some(), model.is_some());
                assert_eq!(body.get("model").and_then(Value::as_str), model);
                assert_eq!(body["stream"], streaming);
            }
        }
    }

    #[test]
    fn anthropic_exact_https_preserves_the_messages_path_and_authentication() {
        let protocol = RemoteProtocol::AnthropicExact;
        let endpoint = "https://gateway.example/tenant/V1/Messages";
        for (supplied, destination) in [
            (endpoint, endpoint),
            (
                "HTTPS://Api.Anthropic.Com:443/proxy/V1/Messages/",
                "https://api.anthropic.com/proxy/V1/Messages/",
            ),
            (
                "https://other.anthropic.com/v1/messages/route",
                "https://other.anthropic.com/v1/messages/route",
            ),
        ] {
            assert_eq!(remote_destination(supplied, protocol).unwrap(), destination);
        }
        for invalid in [
            "http://gateway.example/v1/messages",
            "https://gateway.example/tenant/../v1/messages",
            "https://gateway.example/tenant//v1/messages",
            "https://gateway.example/v1/%6dessages",
            "https://gateway.example/v1/messages?credential=value",
            "https://gateway.example/v1/messages#fragment",
            "https://user@gateway.example/v1/messages",
            "https://gateway.example/v1/messages/other",
            "https://api.anthropic.com.example/v1/messages/",
            "https://%61pi.anthropic.com/v1/messages/",
        ] {
            assert!(remote_destination(invalid, protocol).is_err(), "{invalid}");
        }
        assert!(super::super::discover_remote(endpoint, protocol, None).is_err());
        let options = ProfileOptions::default();
        assert!(Profile::openai_with_server_default(endpoint, protocol, options.clone()).is_err());
        let profile = Profile::remote(endpoint, "fixture/exact", protocol, options).unwrap();
        assert!(profile.uses_anthropic_protocol());
        let Target::Remote(base, route) = &profile.target else {
            panic!("expected an exact HTTPS Messages endpoint");
        };
        assert_eq!(format!("{base}{}", route.completion_path()), endpoint);
        let (name, header) = super::super::remote_header("fixture-key", protocol).unwrap();
        assert_eq!(name, "x-api-key");
        assert!(header.is_sensitive());
        let client = Client::new(profile);
        let messages = chat_messages("system", &[], "question");
        let body = anthropic_request(&client, &messages, false, false).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "fixture/exact");
        assert_eq!(request["messages"][0]["content"], "question");
        assert!(request.get("tools").is_none());
    }

    #[test]
    #[ignore = "requires a private loopback HTTPS Messages fixture and a dedicated CA"]
    fn anthropic_exact_https_acceptance_uses_the_exact_messages_route() {
        use crate::local_ai::{Session, test_support::exclusive_run};

        let _exclusive = exclusive_run();
        let endpoint = std::env::var("KORTTY_TEST_EXACT_MESSAGES_ENDPOINT").unwrap();
        let url = url::Url::parse(&endpoint).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.path(), "/tenant/V1/Messages");
        let certificate =
            std::fs::read(std::env::var_os("KORTTY_TEST_EXACT_MESSAGES_CA_DER").unwrap()).unwrap();
        let options = ProfileOptions {
            timeout: Some(std::time::Duration::from_secs(10)),
            credential_env: Some("KORTTY_TEST_EXACT_MESSAGES_KEY".into()),
            ..ProfileOptions::default()
        };
        let profile = Profile::remote(
            &endpoint,
            "fixture/exact",
            RemoteProtocol::AnthropicExact,
            options,
        )
        .unwrap();
        let (mut session, _cancel) = Session::begin(profile).unwrap();
        assert!(
            session
                .client()
                .complete_chat("", &[], "native JSON")
                .is_err()
        );
        session.trust_test_server(&certificate).unwrap();
        assert_eq!(
            session
                .client()
                .complete_chat("", &[], "native JSON")
                .unwrap(),
            "exact JSON"
        );
        assert_eq!(
            session.client().stream_sse_fixture("native SSE").unwrap(),
            "exact SSE"
        );
        assert!(matches!(
            session.client().complete_chat("", &[], "native status"),
            Err(Error::Status(503))
        ));
    }

    #[test]
    #[ignore = "requires a private loopback HTTPS completion fixture and a dedicated CA"]
    fn openai_exact_https_acceptance_verifies_the_certificate_and_preserves_the_wire_route() {
        use crate::local_ai::{Session, test_support::exclusive_run};

        let _exclusive = exclusive_run();
        let endpoint = std::env::var("KORTTY_TEST_EXACT_OPENAI_HTTPS_ENDPOINT")
            .expect("set the private exact completion endpoint");
        let url = url::Url::parse(&endpoint).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.path(), "/tenant/V1/Chat/Completions/");
        let certificate = std::fs::read(
            std::env::var_os("KORTTY_TEST_EXACT_OPENAI_CA_DER").expect("set the private CA path"),
        )
        .unwrap();
        let protocol = RemoteProtocol::OpenAiExact;
        let options = || ProfileOptions {
            timeout: Some(std::time::Duration::from_secs(10)),
            ..ProfileOptions::default()
        };
        let manual = Profile::remote(&endpoint, "fixture/exact", protocol, options()).unwrap();
        {
            let (mut session, _cancel) = Session::begin(manual).unwrap();
            assert!(session.client().complete_chat("", &[], "manual").is_err());
            session.trust_test_server(&certificate).unwrap();
            assert_eq!(
                session.client().complete_chat("", &[], "manual").unwrap(),
                "exact-manual"
            );
        }

        let delegated =
            Profile::openai_with_server_default(&endpoint, protocol, options()).unwrap();
        let (mut session, _cancel) = Session::begin(delegated).unwrap();
        session.trust_test_server(&certificate).unwrap();
        assert_eq!(
            session
                .client()
                .stream_sse_fixture("server default")
                .unwrap(),
            "exact-default"
        );
    }

    #[test]
    fn agent_anthropic_reports_distinct_limits_without_changing_ordinary_stops() {
        for (reason, expected) in [
            ("max_tokens", CompletionLimit::Output),
            ("model_context_window_exceeded", CompletionLimit::Context),
        ] {
            let body = json!({
                "stop_reason": reason,
                "content": [{"type":"text","text":"partial answer"}],
                "usage": {"input_tokens":7,"output_tokens":0},
            });
            let bytes = body.to_string();
            let mut metadata = AgentCompletion::default();
            let mut usage = Usage::default();
            let (answer, _) = anthropic_response_with_stop(
                bytes.as_bytes(),
                false,
                &mut usage,
                Some(&mut metadata),
            )
            .unwrap();
            assert_eq!(answer, "partial answer");
            assert_eq!(metadata.stop, Some(expected));
            assert_eq!(
                (usage.input_tokens, usage.output_tokens),
                (Some(7), Some(0))
            );
            let ordinary = anthropic_response(bytes.as_bytes(), false, &mut Usage::default());
            if expected == CompletionLimit::Context {
                assert!(matches!(
                    ordinary,
                    Err(Error::Invalid("unexpected Anthropic completion reason"))
                ));
            } else {
                assert_eq!(ordinary.unwrap().0, "partial answer");
            }
        }
        let bytes = br#"{"stop_reason":"max_tokens","content":[],"usage":{"output_tokens":2048}}"#;
        let mut metadata = AgentCompletion::default();
        let mut usage = Usage::default();
        assert!(matches!(
            anthropic_response_with_stop(bytes, false, &mut usage, Some(&mut metadata)),
            Err(Error::Invalid("missing Anthropic answer"))
        ));
        assert_eq!(metadata.stop, Some(CompletionLimit::Output));
        assert_eq!(usage.output_tokens, Some(2048));
    }

    #[test]
    fn agent_lm_studio_estimates_limits_only_from_reported_counts_at_the_wire_cap() {
        for (tokens, limit, expected) in [
            (Some(511), Some(512), Some(CompletionLimit::LikelyOutput)),
            (Some(510), Some(512), None),
            (None, Some(512), None),
            (Some(0), Some(1), None),
            (Some(1), Some(1), Some(CompletionLimit::LikelyOutput)),
            (Some(511), Some(0), None),
            (Some(511), None, None),
        ] {
            let body = json!({
                "output": [{"type":"message","content":"partial answer"}],
                "stats": {"total_output_tokens":tokens},
            });
            let mut metadata = AgentCompletion::default();
            let mut usage = Usage::default();
            let answer = lm_response_with_stop(
                body.to_string().as_bytes(),
                limit,
                false,
                &mut usage,
                Some(&mut metadata),
            )
            .unwrap();
            assert_eq!(answer, "partial answer");
            assert_eq!(metadata.stop, expected);
            assert_eq!(usage.output_tokens, tokens);
        }
    }

    #[test]
    fn native_request_preserves_reviewed_history_and_never_enables_server_storage() {
        let client = Client::new(
            Profile::lm_studio(
                "127.0.0.1:1234/api/v1/chat",
                "fixture",
                ProfileOptions::default(),
            )
            .unwrap(),
        );
        let messages = chat_messages("system", &[("question", "answer")], "next");
        let body: Value =
            serde_json::from_slice(&lm_request(&client, &messages, true, false).unwrap()).unwrap();
        let suffix = client.profile.structured_system_suffix().unwrap();
        let reviewed = format!("system{suffix}");
        let reviewed_messages = chat_messages(&reviewed, &[("question", "answer")], "next");
        let reviewed_body: Value =
            serde_json::from_slice(&lm_request(&client, &reviewed_messages, true, false).unwrap())
                .unwrap();
        assert_eq!(body, reviewed_body);
        assert_eq!(reviewed_body["system_prompt"], reviewed);
        assert_eq!(body["store"], false);
        assert!(body.get("integrations").is_none());
        let input = body["input"].as_str().unwrap();
        assert!(input.contains("question") && input.contains("answer") && input.contains("next"));
        assert!(
            body["system_prompt"]
                .as_str()
                .unwrap()
                .contains("valid JSON object")
        );
    }

    #[test]
    fn native_response_excludes_reasoning_and_rejects_capped_or_missing_answers() {
        let body = json!({"output": [{"type":"reasoning","content":"private"},{"type":"message","content":"answer"}], "stats":{"total_output_tokens":9}});
        assert_eq!(lm_result(&body, Some(20), true).unwrap(), "answer");
        assert!(lm_result(&body, Some(10), true).is_err());
        assert!(lm_result(&json!({"output":[]}), None, false).is_err());
    }
}
