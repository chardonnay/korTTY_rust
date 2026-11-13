//! Provider-reported completion metadata. Unknown values remain absent.

use serde_json::Value;
use ureq::http::HeaderMap;
use zeroize::Zeroize;

#[derive(Clone, Default)]
pub(crate) struct Usage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) model: Option<String>,
    pub(crate) request_id: Option<String>,
    pub(super) accounting_unavailable: bool,
}

impl Usage {
    pub(super) fn observe_headers(&mut self, headers: &HeaderMap) {
        if let Some(id) = ["x-request-id", "request-id"]
            .into_iter()
            .find_map(|name| bounded_identifier(headers.get(name)?.to_str().ok()?))
        {
            self.request_id = Some(id);
        }
    }

    pub(super) fn observe_openai(
        &mut self,
        usage: Option<&Value>,
        model: Option<&Value>,
        id: Option<&Value>,
    ) {
        self.observe(usage, "prompt_tokens", "completion_tokens", model, id);
        self.total_tokens = usage
            .and_then(|usage| usage.get("total_tokens"))
            .and_then(Value::as_u64)
            .or(self.total_tokens);
    }

    pub(super) fn observe_anthropic(&mut self, message: &Value) {
        self.observe(
            message.get("usage"),
            "input_tokens",
            "output_tokens",
            message.get("model"),
            message.get("id"),
        );
    }

    pub(super) fn observe_lm(&mut self, response: &Value) {
        self.observe(
            response.get("stats"),
            "input_tokens",
            "total_output_tokens",
            response.get("model_instance_id"),
            response.get("response_id"),
        );
    }

    fn observe(
        &mut self,
        usage: Option<&Value>,
        input: &str,
        output: &str,
        model: Option<&Value>,
        id: Option<&Value>,
    ) {
        if let Some(usage) = usage {
            // The streamed usage fields are cumulative snapshots, including zero.
            self.input_tokens = usage
                .get(input)
                .and_then(Value::as_u64)
                .or(self.input_tokens);
            self.output_tokens = usage
                .get(output)
                .and_then(Value::as_u64)
                .or(self.output_tokens);
        }
        if let Some(model) = model.and_then(Value::as_str).and_then(bounded_identifier) {
            self.model = Some(model);
        }
        if self.request_id.is_none()
            && let Some(id) = id.and_then(Value::as_str).and_then(bounded_identifier)
        {
            self.request_id = Some(id);
        }
    }

    pub(super) fn absorb(&mut self, other: Self) {
        self.input_tokens = other.input_tokens.or(self.input_tokens);
        self.output_tokens = other.output_tokens.or(self.output_tokens);
        self.total_tokens = other.total_tokens.or(self.total_tokens);
        self.model = other.model.or(self.model.take());
        self.request_id = self.request_id.take().or(other.request_id);
    }

    pub(super) fn remove_credential_echo(&mut self, credential: &str) {
        for field in [&mut self.model, &mut self.request_id] {
            if let Some(mut echoed) = field.take_if(|value| value.contains(credential)) {
                echoed.zeroize();
            }
        }
    }

    pub(super) fn reported(self) -> Option<Self> {
        (self.accounting_unavailable
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.total_tokens.is_some()
            || self.model.is_some()
            || self.request_id.is_some())
        .then_some(self)
    }

    pub(crate) fn token_label(&self) -> Option<String> {
        let label = match (self.input_tokens, self.output_tokens) {
            (Some(input), Some(output)) => {
                Some(format!("Reported tokens · {input} input · {output} output"))
            }
            (Some(input), None) => Some(format!(
                "Reported tokens · {input} input · output unavailable"
            )),
            (None, Some(output)) => Some(format!(
                "Reported tokens · input unavailable · {output} output"
            )),
            (None, None) => None,
        };
        if self.accounting_unavailable {
            Some(label.map_or_else(
                || "Usage ledger unavailable".into(),
                |label| format!("{label} · Usage ledger unavailable"),
            ))
        } else {
            label
        }
    }
}

fn bounded_identifier(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        }))
    .then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_and_cumulative_usage_remain_distinct() {
        let mut usage = Usage::default();
        assert!(usage.clone().reported().is_none());
        usage.observe_anthropic(&json!({"id":"msg_test", "model":"claude-fixture", "usage":{"input_tokens":21,"output_tokens":0}}));
        usage.observe_anthropic(&json!({"usage":{"output_tokens":3}}));
        usage.observe_anthropic(&json!({"usage":{"output_tokens":8}}));
        assert_eq!(usage.input_tokens, Some(21));
        assert_eq!(usage.output_tokens, Some(8));
        assert_eq!(usage.model.as_deref(), Some("claude-fixture"));
        assert_eq!(usage.request_id.as_deref(), Some("msg_test"));
        let mut missing = Usage::default();
        missing.observe_lm(&json!({"stats":{"total_output_tokens":0, "input_tokens":"bad"}}));
        assert_eq!(missing.input_tokens, None);
        assert_eq!(missing.output_tokens, Some(0));
        assert_eq!(
            missing.token_label().as_deref(),
            Some("Reported tokens · input unavailable · 0 output")
        );
    }
}
