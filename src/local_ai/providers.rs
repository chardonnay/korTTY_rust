//! Destination validation and native provider wire formats.

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{
    AgentCompletion, Client, CompletionLimit, Error, Profile, RequestContent, RequestMessage,
    Target, Usage, VisionPart,
};

const STRUCTURED_JSON_SYSTEM_SUFFIX: &str =
    "\nReturn only a valid JSON object, without Markdown fences.";

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum RemoteProtocol {
    OpenAi,
    Anthropic,
}

impl RemoteProtocol {
    pub(super) fn completion_path(self) -> &'static str {
        match self {
            Self::OpenAi => "/chat/completions",
            Self::Anthropic => "/messages",
        }
    }
}

impl Profile {
    pub(crate) fn structured_system_suffix(&self) -> Option<&'static str> {
        (self.uses_lm_studio_protocol()
            || matches!(self.target, Target::Remote(_, RemoteProtocol::Anthropic)))
        .then_some(STRUCTURED_JSON_SYSTEM_SUFFIX)
    }
}

pub(crate) fn remote_api_base(endpoint: &str, protocol: RemoteProtocol) -> Result<String, String> {
    let endpoint = endpoint.trim();
    let invalid =
        || "Enter an HTTPS API URL without credentials, a query, or a fragment.".to_owned();
    if endpoint.len() > 2048
        || !endpoint.starts_with("https://")
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
    let path = url.path().trim_end_matches('/');
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
    remote_api_base(endpoint, protocol).map(|base| format!("{base}{}", protocol.completion_path()))
}

pub(super) fn anthropic_request(
    client: &Client,
    messages: &[RequestMessage<'_>],
    json_response: bool,
    stream: bool,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut system = Zeroizing::new(String::new());
    let mut conversation = Vec::new();
    for message in messages {
        let content = match &message.content {
            RequestContent::Text(text) if message.role == "system" => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(text);
                continue;
            }
            RequestContent::Text(text) => json!(text),
            RequestContent::Vision(parts) => {
                let mut blocks = Vec::new();
                for part in parts {
                    let block = match part {
                        VisionPart::Text { text } => json!({"type": "text", "text": text}),
                        VisionPart::ImageUrl { image_url } => {
                            let data = image_url.url.strip_prefix("data:image/png;base64,").ok_or(
                                Error::Invalid("Anthropic image requests require a PNG data URL"),
                            )?;
                            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}})
                        }
                    };
                    blocks.push(block);
                }
                Value::Array(blocks)
            }
        };
        conversation.push(json!({"role": message.role, "content": content}));
    }
    if json_response && !system.ends_with(STRUCTURED_JSON_SYSTEM_SUFFIX) {
        system.push_str(STRUCTURED_JSON_SYSTEM_SUFFIX);
    }
    let mut body = json!({
        "model": client.model(),
        "system": system.as_str(),
        "messages": conversation,
        "max_tokens": client.profile.effective_output_limit(),
        "stream": stream,
    });
    if let Some(effort) = client.profile.options.reasoning_effort.as_deref() {
        if effort == "none" {
            body["thinking"] = json!({"type": "disabled"});
        } else {
            let effort = match effort {
                "minimal" => "low",
                "xhigh" => "max",
                effort => effort,
            };
            body["thinking"] = json!({"type": "adaptive"});
            body["output_config"] = json!({"effort": effort});
        }
    }
    serde_json::to_vec(&body)
        .map(Zeroizing::new)
        .map_err(|_| Error::Service)
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
        "model": client.model(),
        "system_prompt": system,
        "input": input,
        "temperature": 0,
        "store": false,
        "stream": stream,
        "max_output_tokens": client.profile.effective_output_limit(),
    });
    if let Some(effort) = &client.profile.options.reasoning_effort {
        body["reasoning"] = match effort.as_str() {
            "none" => json!("off"),
            "minimal" | "low" => json!("low"),
            "medium" => json!("medium"),
            _ => json!("high"),
        };
    }
    serde_json::to_vec(&body)
        .map(Zeroizing::new)
        .map_err(|_| Error::Service)
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
    use crate::local_ai::{Profile, ProfileOptions, chat_messages};

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
