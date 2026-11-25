//! Typed OpenAI-compatible web-tool turns. Callers retain review and dispatch authority.

mod eligibility;
pub(crate) use eligibility::agent_task_eligible;

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroize;

use super::{
    Client, CompletionLimit, Error, RequestMessage, RequestMessages, Target, UsageAttempt,
    credential_echo, first_choice, mlx_models, streaming,
};

pub(crate) const MAX_CALLS: usize = 3;
const MAX_ARGUMENT_BYTES: usize = 8192;
const MAX_HISTORY_BYTES: usize = 96 * 1024;
// Two executed groups and a final group of tool-round-limit errors fit in the history.
// The orchestrator, rather than this single-turn transport, authorizes execution.
const MAX_HISTORY_ROUNDS: usize = 3;
const MAX_REQUEST_BYTES: usize = 256 * 1024;

pub(crate) struct Request<'a> {
    pub(crate) history: &'a [Message],
    pub(crate) offer: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Call {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) function: Function,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Function {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

impl Drop for Call {
    fn drop(&mut self) {
        self.id.zeroize();
        self.kind.zeroize();
        self.function.name.zeroize();
        self.function.arguments.zeroize();
    }
}

impl fmt::Debug for Call {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Model web-tool call")
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Message {
    Assistant {
        content: Option<String>,
        tool_calls: Vec<Call>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Drop for Message {
    fn drop(&mut self) {
        match self {
            Self::Assistant { content, .. } => content.zeroize(),
            Self::Tool {
                tool_call_id,
                content,
            } => {
                tool_call_id.zeroize();
                content.zeroize();
            }
        }
    }
}

pub(crate) enum Reply {
    Answer(String),
    Calls {
        content: Option<String>,
        calls: Vec<Call>,
        omitted: usize,
    },
}

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Answer(_) => formatter.write_str("Tool-aware model completion"),
            Self::Calls { calls, omitted, .. } => formatter
                .debug_struct("Model web-tool turn")
                .field("calls", &calls.len())
                .field("omitted", omitted)
                .finish(),
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        match self {
            Self::Answer(answer) => answer.zeroize(),
            Self::Calls { content, .. } => content.zeroize(),
        }
    }
}

impl Reply {
    pub(super) fn into_text(mut self) -> Result<String, Error> {
        match &mut self {
            Self::Answer(answer) => Ok(std::mem::take(answer)),
            Self::Calls { .. } => Err(Error::Invalid("unexpected tool call in text completion")),
        }
    }

    fn require_no_credential_echo(&self, credential: Option<&str>) -> Result<(), Error> {
        let Some(credential) = credential else {
            return Ok(());
        };
        let echoed = match self {
            Self::Answer(answer) => credential_echo(answer, credential),
            Self::Calls { content, calls, .. } => {
                content
                    .as_ref()
                    .is_some_and(|text| credential_echo(text, credential))
                    || calls.iter().any(|call| {
                        credential_echo(&call.id, credential)
                            || credential_echo(&call.function.name, credential)
                            || credential_echo(&call.function.arguments, credential)
                    })
            }
        };
        if echoed {
            Err(Error::Review(
                "The model tool response echoed a credential. Review the request again.".into(),
            ))
        } else {
            Ok(())
        }
    }
}

impl Call {
    fn validate(&self) -> Result<(), Error> {
        let identifier = |value: &str, limit| {
            !value.is_empty()
                && value.len() <= limit
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:/".contains(&byte))
        };
        if self.kind != "function"
            || !identifier(&self.id, 128)
            || !identifier(&self.function.name, 128)
            || self.function.arguments.len() > MAX_ARGUMENT_BYTES
            || !serde_json::from_str::<Value>(&self.function.arguments)
                .is_ok_and(|value| value.is_object())
        {
            return Err(Error::Invalid("invalid model web-tool call"));
        }
        Ok(())
    }
}

impl Request<'_> {
    pub(crate) fn context_chars(&self) -> Result<usize, Error> {
        let history = serde_json::to_string(self.history).map_err(|_| Error::Service)?;
        let definitions = if self.offer {
            serde_json::to_string(&Definitions).map_err(|_| Error::Service)?
        } else {
            String::new()
        };
        Ok(history.chars().count() + definitions.chars().count())
    }

    fn validate(&self) -> Result<(), Error> {
        let mut pending = VecDeque::new();
        let mut seen = HashSet::new();
        let mut bytes = 0usize;
        let mut rounds = 0usize;
        if self.history.len() > MAX_HISTORY_ROUNDS * (MAX_CALLS + 1) {
            return Err(Error::Invalid("web-tool history exceeds its message limit"));
        }
        for message in self.history {
            match message {
                Message::Assistant {
                    content,
                    tool_calls,
                } => {
                    rounds += 1;
                    if !pending.is_empty() || tool_calls.is_empty() || tool_calls.len() > MAX_CALLS
                    {
                        return Err(Error::Invalid("web-tool history has unmatched calls"));
                    }
                    bytes += content.as_ref().map_or(0, String::len);
                    for call in tool_calls {
                        call.validate()?;
                        if !seen.insert(call.id.as_str()) {
                            return Err(Error::Invalid("duplicate web-tool call ID"));
                        }
                        bytes += call.id.len()
                            + call.function.name.len()
                            + call.function.arguments.len();
                        pending.push_back(call.id.as_str());
                    }
                }
                Message::Tool {
                    tool_call_id,
                    content,
                } => {
                    if pending.pop_front() != Some(tool_call_id.as_str())
                        || content.trim().is_empty()
                    {
                        return Err(Error::Invalid(
                            "web-tool result does not match its pending call",
                        ));
                    }
                    bytes += tool_call_id.len() + content.len();
                }
            }
            if bytes > MAX_HISTORY_BYTES {
                return Err(Error::Invalid(
                    "web-tool history exceeds its context budget",
                ));
            }
        }
        if !pending.is_empty()
            || rounds > MAX_HISTORY_ROUNDS
            || rounds == MAX_HISTORY_ROUNDS && self.offer
        {
            return Err(Error::Invalid(
                "web-tool history exceeds its round limit or has unmatched calls",
            ));
        }
        Ok(())
    }
}

pub(super) struct Definitions;

impl Serialize for Definitions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let tool = |name: &str, description: &str, argument: &str, detail: &str| {
            json!({
                "type":"function", "function":{
                    "name":name, "description":description,
                    "parameters":{"type":"object", "properties":{argument:{"type":"string","description":detail}}, "required":[argument]}
                }
            })
        };
        [
            tool("web_search", "Search public web sources for current or external information. Cite returned source URLs.", "query", "Public search query."),
            tool("web_extract", "Read one public webpage or document. Cite its source URL when using the result.", "url", "Absolute HTTP or HTTPS URL."),
        ].serialize(serializer)
    }
}

impl Client {
    pub(super) fn complete_web_turn(
        &self,
        initial: &[RequestMessage<'_>],
        web: Request<'_>,
        response_format: bool,
        deadline: Instant,
        mut before_dispatch: impl FnMut() -> Result<(), String>,
        attempt: &mut UsageAttempt,
    ) -> Result<Reply, Error> {
        web.validate()?;
        if !self.profile.supports_web_tool_turns() {
            return Err(Error::Invalid(
                "model web-tool turns require an OpenAI-compatible chat transport",
            ));
        }
        let mut require_current = || {
            super::web_search::require_policy()?;
            before_dispatch()
        };
        super::web_search::require_policy().map_err(Error::Review)?;
        let deadline = if self.profile.starts_sidecar() {
            self.profile.options.request_deadline(Some(deadline))
        } else {
            Some(deadline)
        };
        let transport = self.transport(deadline)?;
        let result = (|| {
            let deadline = self.before_completion_dispatch(
                transport,
                deadline,
                &mut require_current,
                attempt,
            )?;
            let controls = self.openai_reasoning(transport)?;
            let mut request = self.openai_payload(initial, response_format, false, controls)?;
            request.messages = RequestMessages::Web(initial, web.history);
            request.tools = web.offer.then_some(Definitions);
            request.tool_choice = web.offer.then_some("auto");
            if request.encode()?.len() > MAX_REQUEST_BYTES {
                return Err(Error::Invalid("web-tool request exceeds its byte budget"));
            }
            let mut response =
                self.openai_response(transport, request, deadline, &mut require_current, attempt)?;
            let bytes = match response.buffered.take() {
                Some(bytes) => bytes,
                None => self.cancellable_response_bytes(
                    &mut response.response,
                    response.deadline.is_some(),
                )?,
            };
            let choice = first_choice::<String>(&bytes, &mut attempt.usage)?;
            if choice
                .message
                .role
                .as_deref()
                .is_some_and(|role| role != "assistant")
            {
                return Err(Error::Invalid("invalid role in model web-tool response"));
            }
            let reason = choice.finish_reason.as_ref().and_then(Value::as_str);
            if reason == Some("length") {
                if let Some(metadata) = &mut attempt.agent_completion {
                    metadata.stop = Some(CompletionLimit::Output);
                }
                return Err(Error::Invalid(
                    "model web-tool response reached its token limit",
                ));
            }
            if !matches!(reason, None | Some("stop" | "tool_calls"))
                || choice.message.function_call.is_some()
            {
                return Err(Error::Invalid(
                    "unexpected model web-tool completion reason",
                ));
            }
            let mut calls = choice.message.tool_calls.unwrap_or_default();
            let reply = if calls.is_empty() {
                if reason == Some("tool_calls") {
                    return Err(Error::Invalid("model returned an empty web-tool turn"));
                }
                let answer = choice
                    .message
                    .content
                    .ok_or(Error::Invalid("missing message content"))?;
                Reply::Answer(if controls == mlx_models::ReasoningControls::GptOssEffort {
                    streaming::decode_harmony_content(answer)?.0
                } else {
                    answer
                })
            } else {
                if !web.offer {
                    return Err(Error::Invalid("model called a web tool without an offer"));
                }
                let omitted = calls.len().saturating_sub(MAX_CALLS);
                calls.truncate(MAX_CALLS);
                let mut ids: HashSet<&str> = web
                    .history
                    .iter()
                    .filter_map(|message| match message {
                        Message::Assistant { tool_calls, .. } => Some(tool_calls),
                        _ => None,
                    })
                    .flatten()
                    .map(|call| call.id.as_str())
                    .collect();
                for call in &calls {
                    call.validate()?;
                    if !ids.insert(call.id.as_str()) {
                        return Err(Error::Invalid("duplicate web-tool call ID"));
                    }
                }
                Reply::Calls {
                    content: choice.message.content,
                    calls,
                    omitted,
                }
            };
            reply.require_no_credential_echo(attempt.credential.as_deref().map(String::as_str))?;
            require_current().map_err(Error::Review)?;
            if self.completion_credential(attempt)? != attempt.credential {
                return Err(Error::Review(
                    "The model credential changed during the tool turn. Review the request again."
                        .into(),
                ));
            }
            if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                return Err(Error::TimedOut);
            }
            Ok(reply)
        })();
        if let Err(error) = self.validate_transport(transport) {
            attempt.agent_completion = None;
            return Err(error);
        }
        result
    }
}

impl super::Profile {
    #[cfg(all(debug_assertions, feature = "dialog-preview"))]
    pub(crate) fn is_tool_gui_loopback(&self) -> bool {
        matches!(self.target, Target::External(_) | Target::LmStudio(_))
            && self.supports_web_tool_turns()
    }
    pub(crate) fn supports_web_tool_turns(&self) -> bool {
        !self.is_cli()
            && !self.uses_lm_studio_protocol()
            && !self.uses_anthropic_protocol()
            && !matches!(self.target, Target::Remote(_, protocol) if !protocol.is_openai())
            && !matches!(
                self.target,
                Target::Managed(_, super::Purpose::Embedding, _)
            )
    }
}

#[cfg(test)]
mod tests;
