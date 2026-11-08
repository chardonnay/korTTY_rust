//! Shared parsing of optional model reasoning and fenced answers.

use std::sync::LazyLock;

use regex::Regex;
use zeroize::Zeroizing;

pub(crate) fn strip_fence(reply: &str) -> &str {
    if let Some(fenced) = reply.strip_prefix("```")
        && let Some((_, body)) = fenced.split_once('\n')
    {
        return body
            .rsplit_once("```")
            .map_or(body, |(body, _)| body)
            .trim();
    }
    reply
}

pub(crate) fn sanitize_reply(reply: &str) -> Zeroizing<String> {
    static THINK_BLOCK: LazyLock<Regex> =
        LazyLock::new(|| pattern(r"(?is)<think\b[^>]*>.*?</think\s*>"));
    static DANGLING_THINK: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?is)<think\b[^>]*>.*$"));
    static ORPHAN_THINK: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?is)^.*?</think\s*>\s*"));
    static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| pattern(r"\n{3,}"));

    let reply = if reply.contains("<|channel|>") && reply.contains("<|message|>") {
        harmony_answer(reply)
    } else {
        let without_blocks = THINK_BLOCK.replace_all(reply, "");
        let without_tail = DANGLING_THINK.replace_all(&without_blocks, "");
        ORPHAN_THINK.replace(&without_tail, "").into_owned()
    };
    let normalized = reply.replace("\r\n", "\n").replace('\r', "\n");
    Zeroizing::new(
        BLANK_LINES
            .replace_all(normalized.trim(), "\n\n")
            .into_owned(),
    )
}

fn harmony_answer(reply: &str) -> String {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        pattern(r"(?is)<\|channel\|>\s*([a-z_]+)\s*(?:<\|constrain\|>[^<]*)?<\|message\|>")
    });
    static CONTROL: LazyLock<Regex> =
        LazyLock::new(|| pattern(r"(?is)<\|(?:end|return|start|channel|call)\|>"));

    let mut answer = String::new();
    for header in HEADER.captures_iter(reply) {
        if !header[1].eq_ignore_ascii_case("final") {
            continue;
        }
        let body = &reply[header.get(0).expect("whole channel header").end()..];
        let body = CONTROL
            .find(body)
            .map_or(body, |token| &body[..token.start()])
            .trim();
        if !body.is_empty() {
            if !answer.is_empty() {
                answer.push_str("\n\n");
            }
            answer.push_str(body);
        }
    }
    answer
}

fn pattern(expression: &str) -> Regex {
    Regex::new(expression).expect("static model response pattern")
}
