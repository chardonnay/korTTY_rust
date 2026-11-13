use std::fmt::{Display, Write as _};

use serde::Serialize;

use crate::terminal_agent::usage::{Attempt, AttemptKind, AttemptOutcome};

use super::element;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenCount {
    reported: Option<u128>,
    attempts_with_count: u64,
}

impl TokenCount {
    pub(crate) fn new(sum: u128, known: u64) -> Self {
        Self {
            reported: (known != 0).then_some(sum),
            attempts_with_count: known,
        }
    }

    fn label(self, name: &str, attempts: u64) -> String {
        match self.reported {
            None => format!("{name} unavailable"),
            Some(count) if self.attempts_with_count < attempts => format!(
                "{count} {name} (reported subtotal, {}/{attempts} attempts)",
                self.attempts_with_count
            ),
            Some(count) => format!("{count} {name}"),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelUsage {
    pub(crate) attempts: u64,
    pub(crate) prompt_tokens: TokenCount,
    pub(crate) completion_tokens: TokenCount,
    pub(crate) total_tokens: TokenCount,
}

impl ModelUsage {
    pub(super) fn label(&self) -> String {
        let unit = if self.attempts == 1 {
            "attempt"
        } else {
            "attempts"
        };
        format!(
            "Reported tokens across {} model {unit}\n{} · {} · {}",
            self.attempts,
            self.prompt_tokens.label("input", self.attempts),
            self.completion_tokens.label("output", self.attempts),
            self.total_tokens.label("total", self.attempts)
        )
    }

    pub(super) fn write_xml(&self, text: &mut String) {
        text.push_str("      <modelUsage>\n");
        element(text, "        ", "attempts", &self.attempts.to_string());
        for (name, count) in [
            ("promptTokens", self.prompt_tokens),
            ("completionTokens", self.completion_tokens),
            ("totalTokens", self.total_tokens),
        ] {
            let _ = writeln!(text, "        <{name}>");
            optional_number(text, "          ", "reported", count.reported);
            element(
                text,
                "          ",
                "attemptsWithCount",
                &count.attempts_with_count.to_string(),
            );
            let _ = writeln!(text, "        </{name}>");
        }
        text.push_str("      </modelUsage>\n");
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelAttempt {
    kind: &'static str,
    outcome: &'static str,
    elapsed_millis: u128,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl From<&Attempt> for ModelAttempt {
    fn from(attempt: &Attempt) -> Self {
        Self {
            kind: match attempt.kind {
                AttemptKind::Initial => "initial",
                AttemptKind::Repair => "repair",
            },
            outcome: match attempt.outcome {
                AttemptOutcome::Completed => "completed",
                AttemptOutcome::InvalidReply => "invalid reply",
                AttemptOutcome::TokenLimit => "token limit",
                AttemptOutcome::LikelyTokenLimit => "likely token limit",
                AttemptOutcome::Failed => "failed",
                AttemptOutcome::Cancelled => "cancelled",
            },
            elapsed_millis: attempt.elapsed.as_millis(),
            prompt_tokens: attempt.input_tokens,
            completion_tokens: attempt.output_tokens,
            total_tokens: attempt.total_tokens,
        }
    }
}

impl ModelAttempt {
    pub(super) fn label(&self) -> String {
        let field = |count: Option<u64>, name: &str| match count {
            Some(count) => format!("{count} {name}"),
            None => format!("{name} unavailable"),
        };
        format!(
            "{} · {} · {} ms\nReported tokens · {} · {} · {}",
            self.kind,
            self.outcome,
            self.elapsed_millis,
            field(self.prompt_tokens, "input"),
            field(self.completion_tokens, "output"),
            field(self.total_tokens, "total"),
        )
    }

    pub(super) fn write_xml(&self, text: &mut String) {
        text.push_str("        <attempt>\n");
        let indent = "          ";
        element(text, indent, "kind", self.kind);
        element(text, indent, "outcome", self.outcome);
        element(
            text,
            indent,
            "elapsedMillis",
            &self.elapsed_millis.to_string(),
        );
        optional_number(text, indent, "promptTokens", self.prompt_tokens);
        optional_number(text, indent, "completionTokens", self.completion_tokens);
        optional_number(text, indent, "totalTokens", self.total_tokens);
        text.push_str("        </attempt>\n");
    }
}

fn optional_number(text: &mut String, indent: &str, name: &str, number: Option<impl Display>) {
    if let Some(number) = number {
        let _ = writeln!(text, "{indent}<{name}>{number}</{name}>");
    } else {
        let _ = writeln!(text, "{indent}<{name} unavailable=\"true\"/>");
    }
}
