//! Native Messages output limits and compatibility retries.

use std::time::Instant;

use serde_json::{Value, json};
use ureq::{Body, http::Response};
use zeroize::Zeroizing;

use crate::local_ai::{
    Client, DEFAULT_TIMEOUT, Error, Profile, RequestContent, RequestMessage, Transport,
    UsageAttempt, VisionPart, completion_error_bytes,
};

use super::STRUCTURED_JSON_SYSTEM_SUFFIX;

const DEFAULT_OUTPUT: u32 = 4_096;
const BUFFERED_OUTPUT: u32 = 16_000;
const MIN_THINKING: u32 = 1_024;

impl Profile {
    pub(in crate::local_ai) fn anthropic_output_limit(&self) -> u32 {
        let model = self.model().to_ascii_lowercase();
        let known_limit = model.split_once("claude-").and_then(|(_, id)| {
            if ["sonnet-4-5", "haiku-4-5", "opus-4-5"]
                .iter()
                .any(|prefix| id.starts_with(prefix))
            {
                Some(64_000)
            } else if [
                "sonnet-4-6",
                "opus-4-6",
                "opus-4-7",
                "opus-4-8",
                "mythos-5",
                "sonnet-5",
                "fable-5",
                "opus-5",
            ]
            .iter()
            .any(|prefix| id.starts_with(prefix))
            {
                Some(128_000)
            } else {
                None
            }
        });
        let requested = self
            .options
            .max_output_tokens
            .unwrap_or(if known_limit.is_some() {
                BUFFERED_OUTPUT
            } else {
                DEFAULT_OUTPUT
            });
        known_limit.map_or(requested, |limit| requested.min(limit))
    }
}

#[derive(Clone, Copy)]
enum Thinking<'a> {
    Omitted,
    Disabled,
    Adaptive(&'a str),
    Extended(&'a str),
}

impl Thinking<'_> {
    fn budget(self, max_tokens: u32) -> Option<u32> {
        let Self::Extended(effort) = self else {
            return None;
        };
        let configured = match effort {
            "minimal" | "low" => MIN_THINKING,
            "medium" => 4_096,
            "xhigh" => 12_288,
            _ => 8_192,
        };
        let reserve = (max_tokens / 4).max(MIN_THINKING);
        let budget = configured.min(max_tokens.saturating_sub(reserve));
        (budget >= MIN_THINKING).then_some(budget)
    }
}

struct Request<'a> {
    max_tokens: u32,
    thinking: Thinking<'a>,
    used_alternate: bool,
    output_clamped: bool,
}

impl<'a> Request<'a> {
    fn new(profile: &'a Profile, streaming: bool) -> Self {
        let limit = profile.anthropic_output_limit();
        let thinking = match profile.options.reasoning_effort.as_deref() {
            None => Thinking::Omitted,
            Some("none") => Thinking::Disabled,
            Some(effort) if uses_extended_thinking(profile.model()) => Thinking::Extended(effort),
            Some(effort) => Thinking::Adaptive(effort),
        };
        Self {
            max_tokens: if streaming {
                limit
            } else {
                limit.min(BUFFERED_OUTPUT)
            },
            thinking,
            used_alternate: false,
            output_clamped: false,
        }
    }

    fn apply_fields(&self, body: &mut Value) {
        body["max_tokens"] = json!(self.max_tokens);
        match self.thinking {
            Thinking::Omitted => {}
            Thinking::Disabled => body["thinking"] = json!({"type": "disabled"}),
            Thinking::Adaptive(effort) => {
                let effort = match effort {
                    "minimal" => "low",
                    "xhigh" => "max",
                    "on" => "high",
                    effort => effort,
                };
                body["thinking"] = json!({"type": "adaptive"});
                body["output_config"] = json!({"effort": effort});
            }
            Thinking::Extended(_) => {
                if let Some(budget) = self.thinking.budget(self.max_tokens) {
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                }
            }
        }
    }

    fn retry(&mut self, message: &str) -> Result<bool, Error> {
        if !self.output_clamped
            && message.contains("max_tokens")
            && !message.contains("budget_tokens")
            && !["at least", "minimum", "must be >=", "greater than or equal"]
                .iter()
                .any(|part| message.contains(part))
            && let Some(limit) = smaller_limit(message, self.max_tokens)
        {
            self.max_tokens = limit;
            self.output_clamped = true;
            return Ok(true);
        }
        self.adjust_thinking(message)
    }

    fn adjust_thinking(&mut self, message: &str) -> Result<bool, Error> {
        if !(message.contains("thinking")
            || message.contains("budget_tokens")
            || message.contains("output_config"))
            || ![
                "not support",
                "unsupported",
                "unknown",
                "not allowed",
                "invalid",
                "must be",
                "cannot",
                "permitted",
                "unexpected",
            ]
            .iter()
            .any(|part| message.contains(part))
        {
            return Ok(false);
        }
        match self.thinking {
            Thinking::Adaptive(effort)
                if !self.used_alternate
                    && (message.contains("adaptive") || message.contains("output_config"))
                    && Thinking::Extended(effort).budget(self.max_tokens).is_some() =>
            {
                self.thinking = Thinking::Extended(effort);
                self.used_alternate = true;
            }
            Thinking::Extended(effort)
                if !self.used_alternate
                    && self.thinking.budget(self.max_tokens).is_some()
                    && message.contains("adaptive") =>
            {
                self.thinking = Thinking::Adaptive(effort);
                self.used_alternate = true;
            }
            Thinking::Adaptive(_) => self.thinking = Thinking::Omitted,
            Thinking::Extended(_) if self.thinking.budget(self.max_tokens).is_some() => {
                self.thinking = Thinking::Omitted;
            }
            Thinking::Disabled if message.contains("disabled") && message.contains("thinking") => {
                return Err(Error::Invalid(
                    "The Anthropic model rejected explicit thinking Off. Select a supported reasoning setting.",
                ));
            }
            Thinking::Omitted | Thinking::Disabled | Thinking::Extended(_) => return Ok(false),
        }
        Ok(true)
    }
}

fn rejection_message(bytes: &[u8]) -> Option<Zeroizing<String>> {
    let body: Value = serde_json::from_slice(bytes).ok()?;
    if !body
        .get("type")
        .is_none_or(|kind| kind.as_str() == Some("error"))
        || [
            "content",
            "usage",
            "choices",
            "message",
            "completion",
            "stop_reason",
            "delta",
            "output",
        ]
        .iter()
        .any(|field| body.get(field).is_some_and(|value| !value.is_null()))
    {
        return None;
    }
    let error = body.get("error")?;
    if error.get("type").and_then(Value::as_str) != Some("invalid_request_error") {
        return None;
    }
    error
        .get("message")?
        .as_str()
        .map(|message| Zeroizing::new(message.to_ascii_lowercase()))
}

fn uses_extended_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let Some((_, id)) = model.split_once("claude-") else {
        return false;
    };
    if id.starts_with("3-7-sonnet") {
        return true;
    }
    let Some((family, version)) = id.split_once('-') else {
        return false;
    };
    if !matches!(family, "sonnet" | "opus" | "haiku") {
        return false;
    }
    let mut version = version.split('-');
    version.next() == Some("4")
        && version.next().is_none_or(|part| {
            part.parse::<u32>()
                .is_ok_and(|number| number <= 5 || part.len() == 8)
        })
}

fn smaller_limit(message: &str, current: u32) -> Option<u32> {
    let fallback = if current > DEFAULT_OUTPUT {
        DEFAULT_OUTPUT
    } else {
        current / 2
    };
    let limit = message
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|digits| digits.parse::<u32>().ok())
        .filter(|value| (MIN_THINKING..current).contains(value))
        .max()
        .unwrap_or(fallback);
    (limit > 0 && limit < current).then_some(limit)
}

impl Client {
    pub(in crate::local_ai) fn anthropic_completion(
        &self,
        transport: &Transport,
        input: (&[RequestMessage<'_>], bool, bool),
        deadline: Option<Instant>,
        mut before_retry: impl FnMut() -> Result<(), String>,
        attempt: &mut UsageAttempt,
    ) -> Result<(Response<Body>, Instant), Error> {
        let (messages, json_response, streaming) = input;
        let cutoff = Instant::now() + self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let deadline = deadline.map_or(cutoff, |deadline| deadline.min(cutoff));
        let mut request = Request::new(&self.profile, streaming);
        loop {
            let body = encode(self, messages, json_response, streaming, &request)?;
            let mut response =
                self.completion_response(transport, &body, Some(deadline), attempt)?;
            let status = response.status().as_u16();
            if response.status().is_success() {
                return Ok((response, deadline));
            }
            if status != 400 {
                return Err(Error::Status(status));
            }
            let rejection = completion_error_bytes(&mut response);
            if !self.active() {
                return Err(Error::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
            let message = rejection.ok().and_then(|bytes| rejection_message(&bytes));
            if !message
                .as_deref()
                .map_or(Ok(false), |message| request.retry(message))?
            {
                return Err(Error::Status(status));
            }
            self.validate_transport(transport)?;
            if let Some(id) = attempt.usage.request_id.take() {
                drop(Zeroizing::new(id));
            }
            attempt.reviewed_https_credential = Some(attempt.credential.clone());
            self.before_completion_dispatch(transport, Some(deadline), &mut before_retry, attempt)?;
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn request(
    client: &Client,
    messages: &[RequestMessage<'_>],
    json_response: bool,
    streaming: bool,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    encode(
        client,
        messages,
        json_response,
        streaming,
        &Request::new(&client.profile, streaming),
    )
}

fn encode(
    client: &Client,
    messages: &[RequestMessage<'_>],
    json_response: bool,
    streaming: bool,
    request: &Request<'_>,
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
        "stream": streaming,
    });
    request.apply_fields(&mut body);
    serde_json::to_vec(&body)
        .map(Zeroizing::new)
        .map_err(|_| Error::Service)
}
