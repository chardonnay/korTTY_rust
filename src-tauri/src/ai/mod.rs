use crate::model::ai::{
    AiAction, AiExecutionResult, AiProfile, AiRequestPayload, AiTokenUsage, AiTokenUsageSnapshot,
    AiTokenWarningLevel,
};
use anyhow::Result;
use chrono::{Days, NaiveDate, Utc};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::oneshot;

const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 1_800;
const TEST_CONNECT_TIMEOUT_SECS: u64 = 5;
const TEST_REQUEST_TIMEOUT_SECS: u64 = 30;
const CONNECTION_TEST_SYSTEM_PROMPT: &str = "Reply with exactly OK.";
const CONNECTION_TEST_USER_PROMPT: &str = "Connection test.";

#[derive(Debug, Error)]
pub enum AiError {
    #[error("AI API URL must be configured")]
    MissingApiUrl,
    #[error("AI request cancelled")]
    Cancelled,
    #[error("AI API returned status {status}: {message}")]
    ApiStatus { status: u16, message: String },
    #[error("AI API returned an empty response")]
    EmptyResponse,
    #[error("Failed to decode AI response: {0}")]
    InvalidResponse(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub async fn execute_request(
    profile: &AiProfile,
    request: &AiRequestPayload,
) -> Result<AiExecutionResult, AiError> {
    execute_request_internal(profile, request, None).await
}

pub async fn execute_request_with_cancel(
    profile: &AiProfile,
    request: &AiRequestPayload,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<AiExecutionResult, AiError> {
    let mut cancel_rx = cancel_rx;
    execute_request_internal(profile, request, Some(&mut cancel_rx)).await
}

async fn execute_request_internal(
    profile: &AiProfile,
    request: &AiRequestPayload,
    cancel_rx: Option<&mut oneshot::Receiver<()>>,
) -> Result<AiExecutionResult, AiError> {
    if profile.api_url.trim().is_empty() {
        return Err(AiError::MissingApiUrl);
    }

    let client = build_http_client(DEFAULT_CONNECT_TIMEOUT_SECS, DEFAULT_REQUEST_TIMEOUT_SECS)?;
    let request_body = build_standard_request_body(profile, request);
    let request_future = send_request_body(profile, &client, &request_body);
    tokio::pin!(request_future);
    let mut result = if let Some(cancel_rx) = cancel_rx {
        tokio::select! {
            response = &mut request_future => response?,
            _ = cancel_rx => {
                return Err(AiError::Cancelled);
            }
        }
    } else {
        request_future.await?
    };
    if result.content.trim().is_empty() {
        return Err(AiError::EmptyResponse);
    }
    result.active_profile_id = Some(profile.id.clone());
    result.active_profile_name = Some(profile.name.clone());
    Ok(result)
}

pub async fn test_connection(profile: &AiProfile) -> bool {
    if profile.api_url.trim().is_empty() {
        return false;
    }

    let Ok(client) = build_http_client(TEST_CONNECT_TIMEOUT_SECS, TEST_REQUEST_TIMEOUT_SECS) else {
        return false;
    };
    let request_body = build_connection_test_request_body(profile);
    send_request_body(profile, &client, &request_body)
        .await
        .map(|result| !result.content.trim().is_empty())
        .unwrap_or(false)
}

pub fn normalize_profile(profile: &mut AiProfile) {
    if profile.id.trim().is_empty() {
        profile.id = uuid::Uuid::new_v4().to_string();
    }
    profile.name = profile.name.trim().to_string();
    profile.api_url = profile.api_url.trim().to_string();
    profile.model = profile.model.trim().to_string();
    profile.max_selection_chars = profile.max_selection_chars.max(1);
    profile.token_warning_yellow_percent = profile.token_warning_yellow_percent.min(100);
    profile.token_warning_red_percent = profile
        .token_warning_red_percent
        .max(profile.token_warning_yellow_percent)
        .min(100);
    profile.token_reset_period_days = profile.token_reset_period_days.max(1);
}

pub fn refresh_usage(profile: &mut AiProfile) -> AiTokenUsageSnapshot {
    let today = Utc::now().date_naive();
    let reset_days = u64::from(profile.token_reset_period_days.max(1));
    let anchor = parse_date(profile.token_reset_anchor_date.as_deref()).unwrap_or(today);
    let cycle_start = calculate_cycle_start(anchor, reset_days, today);

    let stored_cycle_start =
        parse_date(profile.token_usage_cycle_start_date.as_deref()).unwrap_or(cycle_start);
    if stored_cycle_start != cycle_start {
        profile.used_prompt_tokens = 0;
        profile.used_completion_tokens = 0;
        profile.used_total_tokens = 0;
        profile.token_usage_cycle_start_date = Some(cycle_start.to_string());
    } else if profile.token_usage_cycle_start_date.is_none() {
        profile.token_usage_cycle_start_date = Some(cycle_start.to_string());
    }

    if profile.token_reset_anchor_date.is_none() {
        profile.token_reset_anchor_date = Some(anchor.to_string());
    }

    profile.used_total_tokens = profile.used_total_tokens.max(
        profile
            .used_prompt_tokens
            .saturating_add(profile.used_completion_tokens),
    );

    let max_tokens = resolve_max_tokens(profile);
    let unlimited = max_tokens == 0;
    let warning_level = determine_warning_level(profile, profile.used_total_tokens, max_tokens);
    let next_reset_date = cycle_start
        .checked_add_days(Days::new(reset_days))
        .unwrap_or(cycle_start);

    AiTokenUsageSnapshot {
        used_prompt_tokens: profile.used_prompt_tokens,
        used_completion_tokens: profile.used_completion_tokens,
        used_total_tokens: profile.used_total_tokens,
        max_tokens,
        remaining_tokens: (!unlimited)
            .then_some(max_tokens.saturating_sub(profile.used_total_tokens)),
        cycle_start_date: cycle_start.to_string(),
        next_reset_date: next_reset_date.to_string(),
        warning_level,
        unlimited,
    }
}

pub fn record_usage(profile: &mut AiProfile, usage: &AiTokenUsage) -> AiTokenUsageSnapshot {
    let _ = refresh_usage(profile);
    profile.used_prompt_tokens = profile
        .used_prompt_tokens
        .saturating_add(usage.prompt_tokens);
    profile.used_completion_tokens = profile
        .used_completion_tokens
        .saturating_add(usage.completion_tokens);
    profile.used_total_tokens = profile.used_total_tokens.saturating_add(usage.total_tokens);
    refresh_usage(profile)
}

pub fn resolve_max_tokens(profile: &AiProfile) -> u64 {
    profile
        .token_limit_amount
        .unwrap_or_default()
        .saturating_mul(profile.token_limit_unit.multiplier())
}

fn determine_warning_level(
    profile: &AiProfile,
    used_total_tokens: u64,
    max_tokens: u64,
) -> AiTokenWarningLevel {
    if max_tokens == 0 {
        return AiTokenWarningLevel::None;
    }

    let usage_percent = (used_total_tokens as f64 / max_tokens as f64) * 100.0;
    if usage_percent >= f64::from(profile.token_warning_red_percent) {
        AiTokenWarningLevel::Red
    } else if usage_percent >= f64::from(profile.token_warning_yellow_percent) {
        AiTokenWarningLevel::Yellow
    } else {
        AiTokenWarningLevel::None
    }
}

fn build_system_prompt(request: &AiRequestPayload) -> String {
    let language_code = request
        .response_language_code
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("en");
    format!(
        "You are an assistant that analyzes terminal output. \
Answer in language code {language_code}. \
Use Markdown with short headings and concise, practical content. \
If you need to present tabular data, use Markdown tables and never ASCII-art grid tables. \
Do not invent facts that are not supported by the provided selection. \
If something is uncertain, say so explicitly."
    )
}

fn build_user_prompt(request: &AiRequestPayload) -> String {
    let mut prompt = String::new();
    match request.action {
        AiAction::Summarize => prompt.push_str(
            "Summarize the selected terminal text. Include: overview, key findings, and useful next steps if any.\n",
        ),
        AiAction::SolveProblem => prompt.push_str(
            "Analyze the selected terminal output as an error/problem report. Include: likely cause, concrete fix steps, and safe verification commands.\n",
        ),
        AiAction::Ask => prompt.push_str(
            "Answer the latest user request directly. The latest user request is the current task. Previous assistant replies are reference context only. Do not continue summarizing or continue problem analysis unless the user explicitly asks for that.\n",
        ),
        AiAction::GenerateChatTitle => prompt.push_str(
            "Generate a short, precise title for this AI chat.\nReturn exactly one plain-text line.\nDo not use Markdown, bullets, numbering, or quotation marks.\nKeep it under 80 characters and describe the topic clearly.\n",
        ),
    }
    prompt.push_str("Treat the selected text as the primary source of truth.\n");

    if let Some(connection_name) = request
        .connection_display_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        prompt.push_str("Connection: ");
        prompt.push_str(connection_name.trim());
        prompt.push('\n');
    }

    if matches!(request.action, AiAction::Ask) {
        if let Some(user_prompt) = request
            .user_prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            prompt.push_str("Current user request:\n");
            prompt.push_str(user_prompt.trim());
            prompt.push('\n');
        }
    }

    if matches!(request.action, AiAction::Ask | AiAction::GenerateChatTitle) {
        if let Some(context) = request
            .conversation_context
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            if matches!(request.action, AiAction::Ask) {
                prompt.push_str(
                    "Earlier conversation (reference only; follow the current user request first):\n",
                );
            } else {
                prompt.push_str("Conversation so far:\n");
            }
            prompt.push_str(&to_safe_text_code_block(context));
            prompt.push('\n');
        }
    }

    if matches!(request.action, AiAction::GenerateChatTitle) {
        prompt.push_str("Focus on what the user and AI discussed, not on generic phrasing.\n");
    }

    prompt.push_str("Selected terminal text:\n");
    prompt.push_str(&to_safe_text_code_block(&request.selected_text));
    prompt
}

fn build_http_client(
    connect_timeout_secs: u64,
    request_timeout_secs: u64,
) -> Result<reqwest::Client, AiError> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs))
        .timeout(std::time::Duration::from_secs(request_timeout_secs))
        .build()
        .map_err(AiError::from)
}

fn build_standard_request_body(profile: &AiProfile, request: &AiRequestPayload) -> Value {
    build_message_request_body(
        profile,
        &build_system_prompt(request),
        &build_user_prompt(request),
        0.2,
    )
}

fn build_connection_test_request_body(profile: &AiProfile) -> Value {
    build_message_request_body(
        profile,
        CONNECTION_TEST_SYSTEM_PROMPT,
        CONNECTION_TEST_USER_PROMPT,
        0.0,
    )
}

fn build_message_request_body(
    profile: &AiProfile,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f64,
) -> Value {
    serde_json::json!({
        "model": (!profile.model.trim().is_empty()).then_some(profile.model.trim()),
        "messages": [
            {
                "role": "system",
                "content": system_prompt,
            },
            {
                "role": "user",
                "content": user_prompt,
            }
        ],
        "temperature": temperature
    })
}

async fn send_request_body(
    profile: &AiProfile,
    client: &reqwest::Client,
    request_body: &Value,
) -> Result<AiExecutionResult, AiError> {
    let mut builder = client
        .post(profile.api_url.trim())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(request_body);

    if !profile.api_key.trim().is_empty() {
        builder = builder.bearer_auth(profile.api_key.trim());
    }

    let response = builder.send().await?;
    let status = response.status();
    let response_body = response.text().await?;
    if !status.is_success() {
        return Err(AiError::ApiStatus {
            status: status.as_u16(),
            message: extract_error_message(&response_body),
        });
    }

    parse_response_body(&response_body)
}

fn to_safe_text_code_block(text: &str) -> String {
    let mut fence = "```".to_string();
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}text\n{text}\n{fence}")
}

fn parse_response_body(response_body: &str) -> Result<AiExecutionResult, AiError> {
    if let Ok(root) = serde_json::from_str::<Value>(response_body) {
        if let Some(result) = parse_json_response_body(&root) {
            return Ok(result);
        }
    }

    if let Some(candidate_json) = extract_candidate_json(response_body) {
        if let Ok(root) = serde_json::from_str::<Value>(&candidate_json) {
            if let Some(result) = parse_json_response_body(&root) {
                return Ok(result);
            }
        }
    }

    if let Some(content) = extract_json_string_field_lenient(response_body, "content") {
        return Ok(AiExecutionResult {
            content: content.trim().to_string(),
            usage: extract_usage_lenient(response_body),
            usage_snapshot: None,
            active_profile_id: None,
            active_profile_name: None,
        });
    }

    Err(AiError::InvalidResponse(
        "Missing choices[0].message.content".into(),
    ))
}

fn parse_json_response_body(root: &Value) -> Option<AiExecutionResult> {
    let content = extract_content(root)?;
    let usage = root.get("usage").and_then(extract_usage);
    Some(AiExecutionResult {
        content,
        usage,
        usage_snapshot: None,
        active_profile_id: None,
        active_profile_name: None,
    })
}

fn extract_content(root: &Value) -> Option<String> {
    let message_content = root
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?;

    if let Some(content) = message_content.as_str() {
        return Some(content.trim().to_string());
    }

    if let Some(parts) = message_content.as_array() {
        let text = parts
            .iter()
            .filter_map(|part| {
                part.as_object()
                    .and_then(|object| object.get("text"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return Some(text.trim().to_string());
        }
    }

    None
}

fn extract_usage(usage: &Value) -> Option<AiTokenUsage> {
    Some(AiTokenUsage {
        prompt_tokens: usage.get("prompt_tokens")?.as_u64()?,
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

fn extract_candidate_json(response_body: &str) -> Option<String> {
    let response_body = if let Some(prediction_index) = response_body.find("Generated prediction:")
    {
        &response_body[prediction_index..]
    } else {
        response_body
    };
    let first_brace = response_body.find('{')?;
    let last_brace = response_body.rfind('}')?;
    (last_brace > first_brace).then(|| response_body[first_brace..=last_brace].to_string())
}

fn extract_usage_lenient(response_body: &str) -> Option<AiTokenUsage> {
    let prompt_tokens =
        extract_u64_field_lenient(response_body, "prompt_tokens").unwrap_or_default();
    let completion_tokens =
        extract_u64_field_lenient(response_body, "completion_tokens").unwrap_or_default();
    let total_tokens = extract_u64_field_lenient(response_body, "total_tokens")
        .unwrap_or(prompt_tokens.saturating_add(completion_tokens));

    if prompt_tokens == 0 && completion_tokens == 0 && total_tokens == 0 {
        return None;
    }

    Some(AiTokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    })
}

fn extract_u64_field_lenient(source: &str, field_name: &str) -> Option<u64> {
    let marker = format!("\"{field_name}\"");
    let field_index = source.find(&marker)?;
    let colon_index = source[field_index + marker.len()..].find(':')? + field_index + marker.len();
    let digits = source[colon_index + 1..]
        .chars()
        .skip_while(|character| character.is_whitespace())
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn extract_json_string_field_lenient(source: &str, field_name: &str) -> Option<String> {
    if source.trim().is_empty() || field_name.trim().is_empty() {
        return None;
    }

    let marker = format!("\"{field_name}\"");
    let field_index = source.find(&marker)?;
    let colon_index = source[field_index + marker.len()..].find(':')? + field_index + marker.len();
    let mut value_start = colon_index + 1;

    while let Some(character) = source[value_start..].chars().next() {
        if character.is_whitespace() {
            value_start += character.len_utf8();
            continue;
        }
        break;
    }

    let remaining = &source[value_start..];
    if !remaining.starts_with('"') {
        return None;
    }

    decode_json_string_lenient(&remaining[1..])
}

fn decode_json_string_lenient(source: &str) -> Option<String> {
    let mut builder = String::new();
    let mut chars = source.chars();

    while let Some(character) = chars.next() {
        match character {
            '"' => return Some(builder),
            '\\' => match chars.next() {
                Some('"') => builder.push('"'),
                Some('\\') => builder.push('\\'),
                Some('/') => builder.push('/'),
                Some('b') => builder.push('\u{0008}'),
                Some('f') => builder.push('\u{000C}'),
                Some('n') => builder.push('\n'),
                Some('r') => builder.push('\r'),
                Some('t') => builder.push('\t'),
                Some('u') => {
                    let hex = chars.by_ref().take(4).collect::<String>();
                    if hex.len() != 4 {
                        builder.push_str("\\u");
                        builder.push_str(&hex);
                        continue;
                    }
                    match u16::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(|value| char::from_u32(u32::from(value)))
                    {
                        Some(decoded) => builder.push(decoded),
                        None => {
                            builder.push_str("\\u");
                            builder.push_str(&hex);
                        }
                    }
                }
                Some(other) => builder.push(other),
                None => {
                    builder.push('\\');
                    return Some(builder);
                }
            },
            other => builder.push(other),
        }
    }

    None
}

fn extract_error_message(response_body: &str) -> String {
    serde_json::from_str::<Value>(response_body)
        .ok()
        .and_then(|root| root.get("error").cloned())
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| value.as_str().map(ToString::to_string))
        })
        .unwrap_or_else(|| response_body.trim().to_string())
}

fn parse_date(value: Option<&str>) -> Option<NaiveDate> {
    value
        .filter(|date| !date.trim().is_empty())
        .and_then(|date| NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok())
}

fn calculate_cycle_start(anchor: NaiveDate, reset_days: u64, today: NaiveDate) -> NaiveDate {
    if today <= anchor {
        return anchor;
    }
    let days_between = today.signed_duration_since(anchor).num_days() as u64;
    let cycles = days_between / reset_days.max(1);
    anchor
        .checked_add_days(Days::new(cycles.saturating_mul(reset_days.max(1))))
        .unwrap_or(anchor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ai::{AiTokenLimitUnit, AiTokenizerType};

    #[test]
    fn builds_safe_code_block() {
        let text = "hello\n```nested```";
        let block = to_safe_text_code_block(text);
        assert!(block.starts_with("````text"));
        assert!(block.contains(text));
    }

    #[test]
    fn builds_dedicated_connection_test_request_body() {
        let profile = AiProfile {
            id: "p1".into(),
            name: "Profile".into(),
            api_url: "http://localhost".into(),
            model: "gpt-4.1-mini".into(),
            ..AiProfile::default()
        };

        let body = build_connection_test_request_body(&profile);
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-4.1-mini")
        );
        assert_eq!(body.get("temperature").and_then(Value::as_f64), Some(0.0));
        assert_eq!(
            body.pointer("/messages/0/content").and_then(Value::as_str),
            Some(CONNECTION_TEST_SYSTEM_PROMPT)
        );
        assert_eq!(
            body.pointer("/messages/1/content").and_then(Value::as_str),
            Some(CONNECTION_TEST_USER_PROMPT)
        );
    }

    #[test]
    fn parses_wrapped_json_response_body_leniently() {
        let response_body = r#"noise before {"choices":[{"message":{"content":"OK"}}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}} noise after"#;
        let result = parse_response_body(response_body).expect("wrapped JSON should parse");
        assert_eq!(result.content, "OK");
        let usage = result.usage.expect("usage should parse");
        assert_eq!(usage.prompt_tokens, 1);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, 3);
    }

    #[test]
    fn ask_prompt_prioritizes_latest_user_request() {
        let request = AiRequestPayload {
            action: AiAction::Ask,
            profile_id: "profile".into(),
            selected_text: "terminal output".into(),
            connection_display_name: Some("server".into()),
            response_language_code: Some("de".into()),
            user_prompt: Some("Explain step 2 in more detail.".into()),
            conversation_context: Some("Assistant: Summary text".into()),
        };

        let prompt = build_user_prompt(&request);
        assert!(prompt.contains("Current user request:\nExplain step 2 in more detail."));
        assert!(prompt.contains(
            "Earlier conversation (reference only; follow the current user request first):"
        ));

        let request_position = prompt
            .find("Current user request:")
            .expect("current request should be included");
        let context_position = prompt
            .find("Earlier conversation (reference only; follow the current user request first):")
            .expect("conversation context should be included");
        assert!(request_position < context_position);
        assert!(!prompt.contains("Summarize the selected terminal text."));
    }

    #[test]
    fn refreshes_and_resets_usage_cycles() {
        let mut profile = AiProfile {
            id: "p1".into(),
            name: "Profile".into(),
            api_url: "http://localhost".into(),
            model: "gpt".into(),
            api_key: String::new(),
            max_selection_chars: 100,
            tokenizer_type: AiTokenizerType::Estimate,
            token_limit_amount: Some(10),
            token_limit_unit: AiTokenLimitUnit::Thousands,
            token_warning_yellow_percent: 75,
            token_warning_red_percent: 90,
            token_reset_period_days: 30,
            token_reset_anchor_date: Some("2025-01-01".into()),
            token_usage_cycle_start_date: Some("2024-12-01".into()),
            used_prompt_tokens: 10,
            used_completion_tokens: 20,
            used_total_tokens: 30,
        };

        let snapshot = refresh_usage(&mut profile);
        assert_eq!(snapshot.used_total_tokens, 0);
        assert_eq!(snapshot.max_tokens, 10_000);
        assert!(!snapshot.unlimited);
    }
}
