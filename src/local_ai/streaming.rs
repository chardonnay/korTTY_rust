//! Bounded SSE decoding shared by live chat transports.

use std::io::{BufRead, BufReader, Read};
use std::time::Instant;

use serde_json::Value;
use zeroize::Zeroizing;

use super::policy_profile::{EchoMask, redact_echo};
use super::{
    Client, DEFAULT_TIMEOUT, Error, MAX_RESPONSE_BYTES, RemoteProtocol, RequestMessage, Target,
    Usage, UsageAttempt, chat_messages, completion_status, is_event_stream, providers,
};

mod harmony;

const MAX_WIRE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) enum StreamDelta {
    Text(String),
    Reasoning(String),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Protocol {
    OpenAi,
    LmStudio,
    Anthropic,
}

#[derive(Clone, Copy)]
enum Mode {
    Chat,
    #[cfg(test)]
    SseFixture,
    Structured {
        response_format: bool,
        deadline: Option<Instant>,
    },
    ReviewedText {
        deadline: Instant,
    },
}

impl Client {
    pub(crate) fn stream_chat(
        &self,
        system: &str,
        history: &[(&str, &str)],
        input: &str,
        before_dispatch: impl FnMut() -> Result<(), String>,
        mut emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        let messages = chat_messages(system, history, input);
        self.with_completion(|attempt| {
            self.stream_messages(&messages, Mode::Chat, before_dispatch, &mut emit, attempt)
        })
    }

    #[cfg(test)]
    pub(crate) fn stream_sse_fixture(&self, input: &str) -> Result<String, Error> {
        let messages = chat_messages("", &[], input);
        self.with_completion(|attempt| {
            self.stream_messages(&messages, Mode::SseFixture, || Ok(()), &mut |_| {}, attempt)
        })
    }

    /// Streams a structured analysis response while retaining the JSON-object request mode.
    pub(crate) fn stream_analysis(
        &self,
        system: &str,
        input: &str,
        before_dispatch: impl FnMut() -> Result<(), String>,
        emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        self.stream_structured(system, input, true, None, before_dispatch, emit)
    }

    pub(crate) fn stream_structured(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Option<Instant>,
        before_dispatch: impl FnMut() -> Result<(), String>,
        emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        self.stream_structured_bound(
            [system, input],
            (response_format, deadline),
            None,
            before_dispatch,
            emit,
        )
    }

    pub(crate) fn stream_reviewed_structured_before(
        &self,
        messages: [&str; 2],
        response_format: bool,
        deadline: Instant,
        credential: Option<&str>,
        before_dispatch: impl FnMut() -> Result<(), String>,
        emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        if !self.profile.is_remote() {
            return Err(Error::Invalid("choose a reviewed HTTPS profile"));
        }
        self.stream_structured_bound(
            messages,
            (response_format, Some(deadline)),
            Some(credential),
            before_dispatch,
            emit,
        )
    }

    fn stream_structured_bound(
        &self,
        [system, input]: [&str; 2],
        (response_format, deadline): (bool, Option<Instant>),
        reviewed_credential: Option<Option<&str>>,
        before_dispatch: impl FnMut() -> Result<(), String>,
        mut emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        let messages = chat_messages(system, &[], input);
        self.with_completion(|attempt| {
            attempt.reviewed_https_credential = reviewed_credential
                .map(|credential| credential.map(|value| Zeroizing::new(value.to_owned())));
            self.stream_messages(
                &messages,
                Mode::Structured {
                    response_format,
                    deadline,
                },
                before_dispatch,
                &mut emit,
                attempt,
            )
        })
    }

    /// Streams a reviewed plain-text request while retaining its credential and deadline.
    pub(crate) fn stream_reviewed_text_before(
        &self,
        [system, input]: [&str; 2],
        deadline: Instant,
        credential: Option<Zeroizing<String>>,
        before_dispatch: impl FnMut() -> Result<(), String>,
        mut emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        let messages = chat_messages(system, &[], input);
        self.with_completion(|attempt| {
            if self.profile.is_remote() || self.profile.is_loopback_anthropic() {
                attempt.reviewed_https_credential = Some(credential);
            }
            self.stream_messages(
                &messages,
                Mode::ReviewedText { deadline },
                before_dispatch,
                &mut emit,
                attempt,
            )
        })
    }

    fn stream_messages(
        &self,
        messages: &[RequestMessage<'_>],
        mode: Mode,
        mut before_dispatch: impl FnMut() -> Result<(), String>,
        emit: &mut impl FnMut(StreamDelta),
        attempt: &mut UsageAttempt,
    ) -> Result<String, Error> {
        if !self.active() {
            return Err(Error::Cancelled);
        }
        if matches!(
            self.profile.target,
            Target::Managed(_, super::Purpose::Embedding, _)
        ) {
            return Err(Error::Invalid(
                "an embedding model cannot complete a chat request",
            ));
        }
        let protocol = if self.profile.uses_lm_studio_protocol() {
            Protocol::LmStudio
        } else if self.profile.uses_anthropic_protocol() {
            Protocol::Anthropic
        } else {
            Protocol::OpenAi
        };
        let (structured, response_format, deadline) = match mode {
            Mode::Chat => (false, false, None),
            #[cfg(test)]
            Mode::SseFixture => (false, false, None),
            Mode::Structured {
                response_format,
                deadline,
            } => (true, response_format, deadline),
            Mode::ReviewedText { deadline } => (true, false, Some(deadline)),
        };
        self.require_text_for_mlx(messages)?;
        let response_format = response_format && !self.profile.is_mlx();
        let deadline = if structured && self.profile.starts_sidecar() {
            let limit = Instant::now() + self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
            Some(deadline.map_or(limit, |deadline| deadline.min(limit)))
        } else {
            deadline
        };
        let transport = self.transport(deadline)?;
        if protocol == Protocol::LmStudio {
            self.require_lm_studio_reasoning(deadline, attempt, &mut before_dispatch)?;
        }
        let deadline =
            self.before_completion_dispatch(transport, deadline, &mut before_dispatch, attempt)?;
        let mut decode_harmony = false;
        let (mut response, buffered, deadline) = if protocol == Protocol::OpenAi {
            let controls = self.openai_reasoning(transport)?;
            decode_harmony = controls == super::mlx_models::ReasoningControls::GptOssEffort;
            let request = self.openai_payload(messages, response_format, true, controls)?;
            let response = self
                .openai_response(transport, request, deadline, before_dispatch, attempt)
                .or_else(|error| {
                    self.validate_transport(transport)?;
                    Err(error)
                })?;
            (response.response, response.buffered, response.deadline)
        } else if protocol == Protocol::Anthropic {
            let (response, deadline) = self.anthropic_completion(
                transport,
                (messages, response_format, true),
                deadline,
                before_dispatch,
                attempt,
            )?;
            (response, None, Some(deadline))
        } else {
            let body = providers::lm_request(self, messages, response_format, true)?;
            let response = self.completion_response(transport, &body, deadline, attempt)?;
            completion_status(&response)?;
            (response, None, deadline)
        };
        #[cfg(test)]
        if matches!(mode, Mode::SseFixture) && !is_event_stream(&response) {
            return Err(Error::Invalid("fixture did not return an event stream"));
        }
        let credential = attempt.credential.as_deref().map(String::as_str);
        let result = if is_event_stream(&response) {
            let mut state = State {
                require_openai_stop: matches!(mode, Mode::ReviewedText { .. }),
                native_minimax: matches!(
                    self.profile.target,
                    Target::Remote(_, RemoteProtocol::MiniMaxNative)
                ),
                harmony: decode_harmony.then(harmony::Decoder::default),
                ..State::default()
            };
            let mut text = EchoMask::default();
            let mut reasoning = EchoMask::default();
            let result = read_stream(
                response.body_mut().as_reader(),
                protocol,
                Some(self.profile.effective_output_limit()),
                || self.active(),
                &mut |delta| {
                    let masked = match delta {
                        StreamDelta::Text(part) => StreamDelta::Text(text.push(part, credential)),
                        StreamDelta::Reasoning(part) => {
                            StreamDelta::Reasoning(reasoning.push(part, credential))
                        }
                    };
                    emit_nonempty(emit, masked);
                },
                &mut state,
            );
            if result.is_ok() {
                emit_nonempty(emit, StreamDelta::Reasoning(reasoning.finish()));
                emit_nonempty(emit, StreamDelta::Text(text.finish()));
            }
            attempt.usage.absorb(state.usage);
            result
        } else {
            let bytes = match buffered {
                Some(bytes) => bytes,
                None => self.cancellable_response_bytes(
                    &mut response,
                    deadline.is_some() || self.profile.options.timeout.is_some(),
                )?,
            };
            let answer = match protocol {
                Protocol::LmStudio => providers::lm_response(
                    &bytes,
                    Some(self.profile.effective_output_limit()),
                    true,
                    &mut attempt.usage,
                )?,
                Protocol::Anthropic => {
                    let (answer, reasoning) =
                        providers::anthropic_response(&bytes, true, &mut attempt.usage)?;
                    if !structured && !reasoning.is_empty() {
                        emit(StreamDelta::Reasoning(redact_echo(reasoning, credential)));
                    }
                    answer
                }
                Protocol::OpenAi => {
                    let choice = super::first_choice::<Value>(&bytes, &mut attempt.usage)?;
                    match choice.finish_reason.as_ref().and_then(Value::as_str) {
                        Some("length") => {
                            return Err(Error::Invalid("model output reached its token limit"));
                        }
                        Some(reason) if structured && reason != "stop" => {
                            return Err(Error::Invalid("unexpected completion reason"));
                        }
                        _ => {}
                    }
                    match choice.message.content {
                        Some(Value::String(text)) => text,
                        Some(Value::Array(parts)) if matches!(mode, Mode::ReviewedText { .. }) => {
                            parts
                                .iter()
                                .filter_map(|part| part.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                        _ => return Err(Error::Invalid("missing message content")),
                    }
                }
            };
            let answer = if decode_harmony {
                let (answer, analysis) = decode_harmony_content(answer)?;
                if !structured {
                    emit_nonempty(
                        emit,
                        StreamDelta::Reasoning(redact_echo(analysis, credential)),
                    );
                }
                answer
            } else {
                answer
            };
            if !structured {
                emit(StreamDelta::Text(redact_echo(answer.clone(), credential)));
            }
            Ok(answer)
        };
        self.validate_transport(transport)?;
        result.and_then(|answer| self.completion_answer(answer, credential, structured))
    }
}

impl super::Profile {
    pub(crate) fn completion_secret(&self) -> Option<Zeroizing<String>> {
        if self.policy_profile.is_some() {
            return self.policy_credential().ok().flatten();
        }
        self.options
            .credential_env
            .as_deref()
            .and_then(|name| super::credential_value(name).ok())
    }
}

fn emit_nonempty(emit: &mut impl FnMut(StreamDelta), delta: StreamDelta) {
    let text = match &delta {
        StreamDelta::Text(text) | StreamDelta::Reasoning(text) => text,
    };
    if !text.is_empty() {
        emit(delta);
    }
}

pub(super) fn decode_harmony_content(content: String) -> Result<(String, String), Error> {
    let mut state = State {
        harmony: Some(harmony::Decoder::default()),
        ..State::default()
    };
    state.append_openai(&Zeroizing::new(content), false, &mut |_| {})?;
    let answer = state.final_answer(&mut |_| {})?;
    Ok((answer, std::mem::take(&mut *state.reasoning)))
}

#[cfg(test)]
fn decode(
    reader: impl Read,
    protocol: Protocol,
    limit: Option<u32>,
    active: impl Fn() -> bool,
    emit: &mut impl FnMut(StreamDelta),
) -> Result<(String, Usage), Error> {
    let mut state = State::default();
    let answer = read_stream(reader, protocol, limit, active, emit, &mut state)?;
    Ok((answer, state.usage))
}

fn read_stream(
    reader: impl Read,
    protocol: Protocol,
    limit: Option<u32>,
    active: impl Fn() -> bool,
    emit: &mut impl FnMut(StreamDelta),
    state: &mut State,
) -> Result<String, Error> {
    let mut reader = BufReader::new(reader.take(MAX_WIRE_BYTES + 1));
    let mut line = Zeroizing::new(Vec::new());
    let mut data = Zeroizing::new(String::new());
    let mut bytes = 0u64;
    loop {
        if !active() {
            return Err(Error::Cancelled);
        }
        line.clear();
        // read_until on Take bounds allocation before rejecting an oversized line.
        let read = reader
            .by_ref()
            .take((MAX_EVENT_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|error| {
                if !active() {
                    Error::Cancelled
                } else if error.kind() == std::io::ErrorKind::TimedOut {
                    Error::TimedOut
                } else {
                    Error::Service
                }
            })?;
        bytes += read as u64;
        if bytes > MAX_WIRE_BYTES || line.len() > MAX_EVENT_BYTES {
            return Err(Error::Invalid("stream exceeds its size limit"));
        }
        if read == 0 {
            if !data.is_empty() {
                state.event(&data, protocol, limit, emit)?;
            }
            return if state.finished || state.native_minimax && state.openai_stopped {
                state.final_answer(emit)
            } else {
                Err(Error::Invalid("stream ended before completion"))
            };
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| Error::Invalid("invalid UTF-8 stream"))?
            .trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data.is_empty() {
                state.event(&data, protocol, limit, emit)?;
                data.clear();
                if state.finished {
                    return state.final_answer(emit);
                }
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            if data.len() > MAX_EVENT_BYTES {
                return Err(Error::Invalid("stream event exceeds 64 KiB"));
            }
        }
    }
}

#[derive(Default)]
struct State {
    answer: Zeroizing<String>,
    reasoning: Zeroizing<String>,
    finished: bool,
    require_openai_stop: bool,
    openai_stopped: bool,
    native_minimax: bool,
    anthropic: Anthropic,
    usage: Usage,
    harmony: Option<harmony::Decoder>,
}

#[derive(Default)]
struct Anthropic {
    started: bool,
    stopped: bool,
    next_block: u64,
    active: Option<Block>,
}

#[derive(Clone, Copy)]
enum Block {
    Text,
    Thinking,
    RedactedThinking,
}

impl State {
    fn final_answer(&mut self, emit: &mut impl FnMut(StreamDelta)) -> Result<String, Error> {
        if let Some(decoder) = self.harmony.take() {
            decoder.finish(|text, reasoning| self.append(text, reasoning, emit))?;
        }
        if self.native_minimax && self.answer.trim().is_empty() {
            return Err(Error::Invalid("missing MiniMax stream answer"));
        }
        Ok(self.answer.to_string())
    }

    fn append_openai(
        &mut self,
        text: &str,
        reasoning: bool,
        emit: &mut impl FnMut(StreamDelta),
    ) -> Result<(), Error> {
        if !reasoning && let Some(mut decoder) = self.harmony.take() {
            decoder.push(text, |text, reasoning| self.append(text, reasoning, emit))?;
            self.harmony = Some(decoder);
            return Ok(());
        }
        self.append(text, reasoning, emit)
    }

    fn append(
        &mut self,
        text: &str,
        reasoning: bool,
        emit: &mut impl FnMut(StreamDelta),
    ) -> Result<(), Error> {
        let target = if reasoning {
            &mut self.reasoning
        } else {
            &mut self.answer
        };
        if target.len().saturating_add(text.len()) > MAX_RESPONSE_BYTES as usize {
            return Err(Error::Invalid("streamed content exceeds 256 KiB"));
        }
        target.push_str(text);
        if !text.is_empty() {
            emit(if reasoning {
                StreamDelta::Reasoning(text.into())
            } else {
                StreamDelta::Text(text.into())
            });
        }
        Ok(())
    }

    fn event(
        &mut self,
        data: &str,
        protocol: Protocol,
        limit: Option<u32>,
        emit: &mut impl FnMut(StreamDelta),
    ) -> Result<(), Error> {
        if protocol == Protocol::OpenAi && data.trim() == "[DONE]" {
            if self.require_openai_stop && !self.openai_stopped {
                return Err(Error::Invalid("stream ended before completion"));
            }
            self.finished = true;
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(data).map_err(|_| Error::Invalid("invalid stream JSON"))?;
        if protocol == Protocol::Anthropic {
            return self.anthropic_event(&value, emit);
        }
        if protocol == Protocol::LmStudio {
            match value.get("type").and_then(Value::as_str) {
                Some("message.start") if !self.answer.is_empty() => {
                    self.append("\n\n", false, emit)?
                }
                Some("message.delta" | "reasoning.delta") => {
                    let reasoning = value["type"] == "reasoning.delta";
                    let text = value
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or(Error::Invalid("invalid native stream content"))?;
                    self.append(text, reasoning, emit)?;
                }
                Some(
                    "error"
                    | "tool_call.start"
                    | "tool_call.arguments"
                    | "tool_call.success"
                    | "tool_call.failure",
                ) => return Err(Error::Invalid("unexpected tool or error in chat stream")),
                Some("chat.end") => {
                    let result = value
                        .get("result")
                        .ok_or(Error::Invalid("missing native stream result"))?;
                    self.usage.observe_lm(result);
                    let answer = providers::lm_result(result, limit, true)?;
                    if self.answer.is_empty() {
                        self.append(&answer, false, emit)?;
                    } else if *self.answer != answer {
                        return Err(Error::Invalid("stream answer differs from final response"));
                    }
                    self.finished = true;
                }
                Some(_) => {}
                None => return Err(Error::Invalid("missing native stream event type")),
            }
            return Ok(());
        }
        self.usage
            .observe_openai(value.get("usage"), value.get("model"), value.get("id"));
        let choices = value.get("choices").and_then(Value::as_array);
        if let Some(error) = providers::openai_provider_error(
            value.get("error"),
            value.get("base_resp"),
            choices.is_some_and(|choices| !choices.is_empty()),
        ) {
            return Err(error);
        }
        let Some(choices) = choices else {
            return Err(Error::Invalid("missing streamed choices"));
        };
        for choice in choices {
            if self.native_minimax && !choice.is_object() {
                return Err(Error::Invalid("invalid MiniMax stream choice"));
            }
            if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
                continue;
            }
            let already_stopped = self.native_minimax && self.openai_stopped;
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if reason == "length" {
                    return Err(Error::Invalid("model output reached its token limit"));
                }
                if reason != "stop" {
                    return Err(Error::Invalid("unexpected completion reason"));
                }
                self.openai_stopped = true;
            }
            let delta = &choice["delta"];
            let message = &choice["message"];
            if openai_tool_call(delta) || self.native_minimax && openai_tool_call(message) {
                return Err(Error::Invalid("unexpected tool call in chat stream"));
            }
            const CONTENT_FIELDS: [(&str, bool); 3] = [
                ("content", false),
                ("reasoning_content", true),
                ("reasoning", true),
            ];
            let cumulative = self.native_minimax
                && message.is_object()
                && !CONTENT_FIELDS.iter().any(|(key, _)| {
                    delta
                        .get(*key)
                        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
                });
            let parts = if cumulative { message } else { delta };
            let mut received = [None, None];
            for (key, reasoning) in CONTENT_FIELDS {
                if let Some(content) = parts.get(key).filter(|value| !value.is_null()) {
                    let text = content
                        .as_str()
                        .ok_or(Error::Invalid("invalid streamed content"))?;
                    // MiniMax native sends cumulative messages, including a final snapshot
                    // after ordinary deltas. Never display the same text twice or revise it.
                    let text = if cumulative {
                        let previous = if reasoning {
                            self.reasoning.as_str()
                        } else {
                            self.answer.as_str()
                        };
                        text.strip_prefix(previous).ok_or(Error::Invalid(
                            "MiniMax stream changed previously received content",
                        ))?
                    } else {
                        text
                    };
                    if already_stopped && !text.is_empty() {
                        return Err(Error::Invalid(
                            "MiniMax stream sent content after completion",
                        ));
                    }
                    if cumulative {
                        // Accept either reasoning field, with reasoning_content preferred.
                        received[usize::from(reasoning)].get_or_insert(text);
                    } else {
                        self.append_openai(text, reasoning, emit)?;
                    }
                }
            }
            if cumulative {
                for (text, reasoning) in received.into_iter().zip([false, true]) {
                    if let Some(text) = text {
                        self.append(text, reasoning, emit)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn anthropic_event(
        &mut self,
        value: &Value,
        emit: &mut impl FnMut(StreamDelta),
    ) -> Result<(), Error> {
        let event = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid("missing Anthropic stream event type"))?;
        if event == "error" {
            return Err(Error::Invalid("server reported an Anthropic stream error"));
        }
        if event == "message_start" && !self.anthropic.started {
            self.anthropic.started = true;
            self.usage.observe_anthropic(&value["message"]);
            return Ok(());
        }
        let index = value.get("index").and_then(Value::as_u64);
        let block = self
            .anthropic
            .active
            .filter(|_| index == Some(self.anthropic.next_block));
        match event {
            "content_block_start"
                if self.anthropic.started
                    && !self.anthropic.stopped
                    && self.anthropic.active.is_none()
                    && index == Some(self.anthropic.next_block) =>
            {
                let content = &value["content_block"];
                let kind = match content["type"].as_str() {
                    Some("text") => Block::Text,
                    Some("thinking") => Block::Thinking,
                    Some("redacted_thinking") => Block::RedactedThinking,
                    _ => return Err(Error::Invalid("unexpected Anthropic stream block")),
                };
                self.anthropic.active = Some(kind);
                let (key, reasoning, previous) = match kind {
                    Block::Text => ("text", false, !self.answer.is_empty()),
                    Block::Thinking => ("thinking", true, !self.reasoning.is_empty()),
                    Block::RedactedThinking => return Ok(()),
                };
                if previous {
                    self.append("\n\n", reasoning, emit)?;
                }
                if let Some(text) = content[key].as_str() {
                    self.append(text, reasoning, emit)?;
                }
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                let (key, reasoning) = match (block, delta["type"].as_str()) {
                    (Some(Block::Text), Some("text_delta")) => ("text", false),
                    (Some(Block::Thinking), Some("thinking_delta")) => ("thinking", true),
                    (Some(Block::Thinking), Some("signature_delta"))
                    | (Some(Block::Text), Some("citations_delta")) => return Ok(()),
                    _ => return Err(Error::Invalid("invalid Anthropic stream delta")),
                };
                self.append(
                    delta[key]
                        .as_str()
                        .ok_or(Error::Invalid("invalid Anthropic stream content"))?,
                    reasoning,
                    emit,
                )?;
            }
            "content_block_stop" if block.is_some() => {
                self.anthropic.active = None;
                self.anthropic.next_block += 1;
            }
            "message_delta" if self.anthropic.started && self.anthropic.active.is_none() => {
                self.usage.observe_anthropic(value);
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    providers::anthropic_stop(reason, true)?;
                    self.anthropic.stopped = true;
                }
            }
            "message_stop" if self.anthropic.stopped && !self.answer.trim().is_empty() => {
                self.finished = true
            }
            "message_start"
            | "content_block_start"
            | "content_block_stop"
            | "message_delta"
            | "message_stop" => {
                return Err(Error::Invalid(
                    "Anthropic stream events arrived out of order",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

fn openai_tool_call(part: &Value) -> bool {
    part.get("tool_calls").is_some() || part.get("function_call").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::{Profile, ProfileOptions, start, test_support};
    use std::io::Write;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    const GPT_OSS_CONTENT: &str = concat!(
        "<|channel|>analysis<|message|>Vérifier 2 < 3.<|end|>",
        "<|start|>assistant<|channel|>commentary<|message|>Working…<|end|>",
        "<|start|>assistant<|channel|>final<|message|>Trois<|return|>"
    );

    #[test]
    fn managed_gpt_oss_decodes_harmony_markers_split_across_events() {
        let mut wire = String::new();
        for character in GPT_OSS_CONTENT.chars() {
            let event = serde_json::json!({
                "choices": [{ "delta": { "content": character.to_string() } }],
            });
            wire.push_str(&format!("data: {event}\n\n"));
        }
        wire.push_str("data: [DONE]\n\n");
        let mut state = State {
            harmony: Some(harmony::Decoder::default()),
            ..State::default()
        };
        let mut answer = String::new();
        let mut reasoning = String::new();
        let decoded = read_stream(
            wire.as_bytes(),
            Protocol::OpenAi,
            None,
            || true,
            &mut |delta| match delta {
                StreamDelta::Text(part) => answer.push_str(&part),
                StreamDelta::Reasoning(part) => reasoning.push_str(&part),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(decoded, "Trois");
        assert_eq!(answer, decoded);
        assert_eq!(reasoning, "Vérifier 2 < 3.");
        assert_eq!(
            decode(
                wire.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |_| {}
            )
            .unwrap()
            .0,
            GPT_OSS_CONTENT
        );
    }

    #[test]
    fn managed_gpt_oss_buffered_responses_require_the_final_channel() {
        assert_eq!(
            decode_harmony_content(GPT_OSS_CONTENT.into()).unwrap(),
            ("Trois".into(), "Vérifier 2 < 3.".into())
        );
        let plain = "3 < 5";
        assert_eq!(
            decode_harmony_content(plain.into()).unwrap(),
            (plain.into(), String::new())
        );
        let analysis_only = "<|channel|>analysis<|message|>Draft<|end|>";
        let incomplete_final = "<|channel|>final<|message|>Three<|ret";
        let tool_call = "<|channel|>commentary to=functions.command<|message|>{}<|call|>";
        for bad in [analysis_only, incomplete_final, tool_call] {
            assert!(decode_harmony_content(bad.into()).is_err());
        }
    }

    #[test]
    fn streaming_handles_utf8_reasoning_and_requires_an_explicit_finish() {
        let wire = b": ping\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"check\"}}]}\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let mut deltas = Vec::new();
        assert_eq!(
            decode(&wire[..], Protocol::OpenAi, None, || true, &mut |delta| {
                deltas.push(delta)
            })
            .unwrap()
            .0,
            "hello"
        );
        assert!(matches!(&deltas[0], StreamDelta::Reasoning(text) if text == "check"));
        assert!(
            decode(
                &wire[..wire.len() - 14],
                Protocol::OpenAi,
                None,
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(matches!(
            decode(&wire[..], Protocol::OpenAi, None, || false, &mut |_| {}),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn minimax_native_stream_emits_only_new_text_from_cumulative_messages() {
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Grü\",\"reasoning_content\":\"check\"}}]}\n\n",
            "data: {\"choices\":[{\"message\":{\"content\":\"Grü\",\"reasoning_content\":\"check\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"message\":{\"content\":\"Grüße\",\"reasoning\":\"check done\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16}}\n\n",
            "data: [DONE]\n\n"
        );
        for complete in [wire, wire.strip_suffix("data: [DONE]\n\n").unwrap()] {
            let mut state = State {
                native_minimax: true,
                require_openai_stop: true,
                ..State::default()
            };
            let mut text = String::new();
            let mut reasoning = String::new();
            let answer = read_stream(
                complete.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |delta| match delta {
                    StreamDelta::Text(delta) => text.push_str(&delta),
                    StreamDelta::Reasoning(delta) => reasoning.push_str(&delta),
                },
                &mut state,
            )
            .unwrap();
            assert_eq!(answer, "Grüße");
            assert_eq!(text, "Grüße");
            assert_eq!(reasoning, "check done");
            assert_eq!(state.usage.total_tokens, Some(16));
        }
        assert_eq!(
            decode(
                wire.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |_| {}
            )
            .unwrap()
            .0,
            "Grü"
        );
        let without_stop = wire.replacen(",\"finish_reason\":\"stop\"", "", 1);
        let mut state = State {
            native_minimax: true,
            require_openai_stop: true,
            ..State::default()
        };
        assert!(matches!(
            read_stream(
                without_stop
                    .strip_suffix("data: [DONE]\n\n")
                    .unwrap()
                    .as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |_| {},
                &mut state,
            ),
            Err(Error::Invalid("stream ended before completion"))
        ));
    }

    #[test]
    fn minimax_native_stream_rejects_rewritten_content_and_message_tool_calls() {
        for (change, reason) in [
            (
                r#"{"content":"changed"}"#,
                "MiniMax stream changed previously received content",
            ),
            (
                r#"{"content":"prefix and tool","tool_calls":[]}"#,
                "unexpected tool call in chat stream",
            ),
        ] {
            let wire = format!(
                "data: {{\"choices\":[{{\"message\":{{\"content\":\"prefix\"}}}}]}}\n\n\
                data: {{\"choices\":[{{\"message\":{change},\"finish_reason\":\"stop\"}}],\"usage\":{{\"total_tokens\":7}}}}\n\n\
                data: [DONE]\n\n"
            );
            let mut state = State {
                native_minimax: true,
                ..State::default()
            };
            let mut text = String::new();
            let result = read_stream(
                wire.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |delta| {
                    if let StreamDelta::Text(delta) = delta {
                        text.push_str(&delta);
                    }
                },
                &mut state,
            );
            assert!(matches!(result, Err(Error::Invalid(actual)) if actual == reason));
            assert_eq!(text, "prefix");
            assert_eq!(state.usage.total_tokens, Some(7));
        }
    }

    #[test]
    fn minimax_native_stream_allows_usage_and_exact_repeats_after_stop() {
        let initial = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\",\"reasoning_content\":\"check\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[{\"message\":{\"content\":\"answer\",\"reasoning_content\":\"check\"}}]}\n\n"
        );
        for (post_stop, allowed) in [
            (r#"{"choices":[],"usage":{"total_tokens":7}}"#, true),
            (
                r#"{"choices":[{"delta":{"content":" late"}}],"usage":{"total_tokens":7}}"#,
                false,
            ),
            (
                r#"{"choices":[{"message":{"reasoning":"check more"}}],"usage":{"total_tokens":7}}"#,
                false,
            ),
        ] {
            let wire = format!("{initial}data: {post_stop}\n\n");
            let mut state = State {
                native_minimax: true,
                ..State::default()
            };
            let mut text = String::new();
            let mut reasoning = String::new();
            let result = read_stream(
                wire.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |delta| match delta {
                    StreamDelta::Text(delta) => text.push_str(&delta),
                    StreamDelta::Reasoning(delta) => reasoning.push_str(&delta),
                },
                &mut state,
            );
            if allowed {
                assert_eq!(result.unwrap(), "answer");
            } else {
                assert!(matches!(
                    result,
                    Err(Error::Invalid(
                        "MiniMax stream sent content after completion"
                    ))
                ));
            }
            assert_eq!(text, "answer");
            assert_eq!(reasoning, "check");
            assert_eq!(state.usage.total_tokens, Some(7));
        }
    }

    #[test]
    fn minimax_native_stream_requires_valid_choices_and_an_answer() {
        for (choices, reason) in [
            (
                r#"[false,{"finish_reason":"stop","message":{"content":"answer"}}]"#,
                "invalid MiniMax stream choice",
            ),
            (
                r#"[{"finish_reason":"stop","message":{"reasoning_content":"check"}}]"#,
                "missing MiniMax stream answer",
            ),
        ] {
            let wire = format!("data: {{\"choices\":{choices}}}\n\n");
            let mut state = State {
                native_minimax: true,
                ..State::default()
            };
            let result = read_stream(
                wire.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |_| {},
                &mut state,
            );
            assert!(matches!(result, Err(Error::Invalid(actual)) if actual == reason));
        }
    }

    #[test]
    fn native_stream_checks_aggregate_and_rejects_limits_and_tools() {
        let wire = concat!(
            "event: message.delta\ndata: {\"type\":\"message.delta\",\"content\":\"answer\"}\n\n",
            "event: chat.end\ndata: {\"type\":\"chat.end\",\"result\":{\"output\":[{\"type\":\"message\",\"content\":\"answer\"}]}}\n\n"
        );
        assert_eq!(
            decode(
                wire.as_bytes(),
                Protocol::LmStudio,
                None,
                || true,
                &mut |_| {}
            )
            .unwrap()
            .0,
            "answer"
        );
        let changed = wire.replace(
            "message\",\"content\":\"answer",
            "message\",\"content\":\"different",
        );
        assert!(
            decode(
                changed.as_bytes(),
                Protocol::LmStudio,
                None,
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        let oversized = format!("data: {}\n\n", "x".repeat(MAX_EVENT_BYTES + 1));
        assert!(
            decode(
                oversized.as_bytes(),
                Protocol::OpenAi,
                None,
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        let tool = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[]}}]}\n\n";
        assert!(decode(&tool[..], Protocol::OpenAi, None, || true, &mut |_| {}).is_err());
    }

    #[test]
    fn anthropic_stream_checks_block_order_and_decodes_split_utf8() {
        struct Split<'a>(&'a [u8]);
        impl Read for Split<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let length = buffer.len().min(1);
                self.0.read(&mut buffer[..length])
            }
        }
        let wire = concat!(
            "data: {\"type\":\"message_start\"}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Consider the question\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Grüße\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let mut deltas = Vec::new();
        let (answer, _) = decode(
            Split(wire.as_bytes()),
            Protocol::Anthropic,
            None,
            || true,
            &mut |delta| deltas.push(delta),
        )
        .unwrap();
        assert_eq!(answer, "Grüße");
        assert!(
            matches!(&deltas[0], StreamDelta::Reasoning(text) if text == "Consider the question")
        );
        for invalid in [
            wire.replace("end_turn", "max_tokens"),
            wire.replace("\"index\":1", "\"index\":4"),
            wire.replace("thinking\",\"thinking\"", "tool_use\",\"thinking\""),
        ] {
            assert!(
                decode(
                    invalid.as_bytes(),
                    Protocol::Anthropic,
                    None,
                    || true,
                    &mut |_| {}
                )
                .is_err()
            );
        }
    }

    #[test]
    fn reviewed_stream_guard_stops_before_connecting_to_the_model() {
        let _exclusive = test_support::exclusive_run();
        let (listener, profile) = test_support::listener();
        let client = Client::new(profile);
        let result = client.stream_chat(
            "system",
            &[],
            "question",
            || Err("The attachment changed.".into()),
            |_| panic!("a rejected guard cannot stream"),
        );
        assert_eq!(result.unwrap_err().message(), "The attachment changed.");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn local_stream_delivers_a_delta_before_the_server_finishes() {
        let _exclusive = test_support::exclusive_run();
        let (listener, profile) = test_support::listener();
        let (delta, observed) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut socket = test_support::accept(&listener);
            let body = test_support::request(&mut socket);
            assert_eq!(body["stream"], true);
            socket.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Request-Id: fixture-request\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"}}]}\n\n").unwrap();
            socket.get_mut().flush().unwrap();
            observed.recv_timeout(Duration::from_secs(3)).unwrap();
            socket.get_mut().write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" second\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4},\"model\":\"fixture-model\"}\n\ndata: [DONE]\n\n").unwrap();
        });
        let client = Client::new(profile);
        let reply = client
            .stream_chat(
                "system",
                &[],
                "question",
                || Ok(()),
                |update| {
                    if matches!(update, StreamDelta::Text(ref text) if text == "first") {
                        delta.send(()).unwrap();
                    }
                },
            )
            .unwrap();
        assert_eq!(reply, "first second");
        let usage = client.take_usage().unwrap();
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.model.as_deref(), Some("fixture-model"));
        assert_eq!(usage.request_id.as_deref(), Some("fixture-request"));
        assert!(client.take_usage().is_none());
        server.join().unwrap();
    }

    #[test]
    fn cancelling_a_stream_closes_the_live_socket_and_releases_the_worker() {
        let _exclusive = test_support::exclusive_run();
        let (listener, profile) = test_support::listener();
        let profile_dir = crate::snippets::tests::Profile::new();
        let account = crate::local_ai::UsageAccount::new(
            profile_dir.0.canonicalize().unwrap(),
            1,
            crate::local_ai::UsageBudget::default(),
        )
        .unwrap();
        let profile = profile.with_usage_account(account.clone());
        let (delta, observed) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut socket = test_support::accept(&listener);
            test_support::request(&mut socket);
            socket.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"usage\":{\"prompt_tokens\":23},\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n").unwrap();
            socket.get_mut().flush().unwrap();
            let result = socket.read(&mut [0]);
            assert!(matches!(result, Ok(0)) || result.is_err());
        });
        let pending = start(profile, move |client| {
            #[cfg(all(target_os = "macos", debug_assertions))]
            assert!(!kortty_macos::AppNapActivity::current_thread_has_activity());
            let result = client
                .stream_chat(
                    "system",
                    &[],
                    "question",
                    || Ok(()),
                    |_| {
                        #[cfg(all(target_os = "macos", debug_assertions))]
                        assert!(kortty_macos::AppNapActivity::current_thread_has_activity());
                        let _ = delta.send(());
                    },
                )
                .map_err(Error::message);
            #[cfg(all(target_os = "macos", debug_assertions))]
            assert!(!kortty_macos::AppNapActivity::current_thread_has_activity());
            result
        })
        .unwrap();
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(pending.cancel);
        assert_eq!(
            pending.reply.recv_blocking().unwrap().unwrap_err(),
            Error::Cancelled.message()
        );
        server.join().unwrap();
        let usage = account.snapshot().unwrap();
        assert_eq!(usage.counts.requests, 1);
        assert_eq!(usage.counts.interrupted, 1);
        assert_eq!(usage.counts.input_tokens, 23);
        assert_eq!(usage.counts.missing_output, 1);
        assert_eq!(usage.in_flight, 0);
    }

    #[test]
    fn lm_studio_native_http_uses_reviewed_stateless_payload() {
        let _exclusive = test_support::exclusive_run();
        let (listener, ordinary) = test_support::listener();
        let profile = Profile::lm_studio(
            &ordinary.external_address().to_string(),
            "fixture",
            ProfileOptions::default(),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let mut socket = test_support::accept(&listener);
            let body = test_support::wire_request(&mut socket, "POST /api/v1/chat HTTP/1.1");
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["store"], false);
            assert_eq!(body["input"], "question");
            let response = test_support::fixture_http_response(
                "200 OK",
                "",
                r#"{"output":[{"type":"reasoning","content":"reason"},{"type":"message","content":"answer"}]}"#,
            );
            socket.get_mut().write_all(response.as_bytes()).unwrap();
        });
        assert_eq!(
            Client::new(profile)
                .complete_chat("system", &[], "question")
                .unwrap(),
            "answer"
        );
        server.join().unwrap();
    }
}
