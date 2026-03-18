use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AiAction {
    Summarize,
    SolveProblem,
    Ask,
    GenerateChatTitle,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AiTokenizerType {
    #[default]
    Estimate,
    Cl100kBase,
    O200kBase,
    P50kBase,
    R50kBase,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AiTokenLimitUnit {
    #[default]
    Thousands,
    Millions,
}

impl AiTokenLimitUnit {
    pub fn multiplier(&self) -> u64 {
        match self {
            Self::Thousands => 1_000,
            Self::Millions => 1_000_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AiTokenWarningLevel {
    None,
    Yellow,
    Red,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfile {
    pub id: String,
    pub name: String,
    pub api_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_max_selection_chars")]
    pub max_selection_chars: usize,
    #[serde(default)]
    pub tokenizer_type: AiTokenizerType,
    pub token_limit_amount: Option<u64>,
    #[serde(default)]
    pub token_limit_unit: AiTokenLimitUnit,
    #[serde(default = "default_yellow_percent")]
    pub token_warning_yellow_percent: u8,
    #[serde(default = "default_red_percent")]
    pub token_warning_red_percent: u8,
    #[serde(default = "default_reset_days")]
    pub token_reset_period_days: u16,
    pub token_reset_anchor_date: Option<String>,
    pub token_usage_cycle_start_date: Option<String>,
    #[serde(default)]
    pub used_prompt_tokens: u64,
    #[serde(default)]
    pub used_completion_tokens: u64,
    #[serde(default)]
    pub used_total_tokens: u64,
}

impl Default for AiProfile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            api_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            max_selection_chars: default_max_selection_chars(),
            tokenizer_type: AiTokenizerType::Estimate,
            token_limit_amount: None,
            token_limit_unit: AiTokenLimitUnit::Thousands,
            token_warning_yellow_percent: default_yellow_percent(),
            token_warning_red_percent: default_red_percent(),
            token_reset_period_days: default_reset_days(),
            token_reset_anchor_date: None,
            token_usage_cycle_start_date: None,
            used_prompt_tokens: 0,
            used_completion_tokens: 0,
            used_total_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTokenUsageSnapshot {
    pub used_prompt_tokens: u64,
    pub used_completion_tokens: u64,
    pub used_total_tokens: u64,
    pub max_tokens: u64,
    pub remaining_tokens: Option<u64>,
    pub cycle_start_date: String,
    pub next_reset_date: String,
    pub warning_level: AiTokenWarningLevel,
    pub unlimited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiExecutionResult {
    pub content: String,
    pub usage: Option<AiTokenUsage>,
    pub usage_snapshot: Option<AiTokenUsageSnapshot>,
    pub active_profile_id: Option<String>,
    pub active_profile_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiRequestPayload {
    pub action: AiAction,
    pub profile_id: String,
    pub selected_text: String,
    pub connection_display_name: Option<String>,
    pub response_language_code: Option<String>,
    pub user_prompt: Option<String>,
    pub conversation_context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAiChat {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub selected_text: String,
    pub connection_display_name: Option<String>,
    pub response_language_code: Option<String>,
    pub active_ai_profile_id: Option<String>,
    pub active_ai_profile_name: Option<String>,
    #[serde(default)]
    pub messages: Vec<SavedAiChatMessage>,
}

impl Default for SavedAiChat {
    fn default() -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "AI Chat".into(),
            created_at: now,
            updated_at: now,
            selected_text: String::new(),
            connection_display_name: None,
            response_language_code: Some("en".into()),
            active_ai_profile_id: None,
            active_ai_profile_name: None,
            messages: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AiChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAiChatMessage {
    pub role: AiChatRole,
    pub content: String,
    pub created_at: i64,
    pub ai_profile_id: Option<String>,
    pub ai_profile_name: Option<String>,
}

fn default_max_selection_chars() -> usize {
    1_000_000
}

fn default_yellow_percent() -> u8 {
    75
}

fn default_red_percent() -> u8 {
    90
}

fn default_reset_days() -> u16 {
    30
}
