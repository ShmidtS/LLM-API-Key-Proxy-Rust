use crate::types::{ContextBudget, GuardrailMode, RouteKind, TokenBudget};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GuardrailsConfig {
    pub enabled: bool,
    pub max_semantic_retries: u32,
    pub chat_completions: RouteGuardrailConfig,
    pub anthropic_messages: RouteGuardrailConfig,
    pub responses: RouteGuardrailConfig,
    pub max_rescue_attempts: u32,
    pub max_guardrail_retries: u32,
    pub context: ContextCompactionConfig,
    pub context_compaction: ContextCompactionConfig,
    pub vram: VramConfig,
    pub trace: GuardrailTraceConfig,
    pub recovery: RecoveryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteGuardrailConfig {
    pub mode: GuardrailMode,
    pub validate_tool_calls: bool,
    pub validate_json_mode: bool,
    pub validate_schema: bool,
    pub validate_steps: bool,
    pub rescue_tool_calls: bool,
    pub retry_with_nudge: bool,
    #[serde(default)]
    pub streaming_enabled: bool,
}

impl Default for RouteGuardrailConfig {
    fn default() -> Self {
        Self {
            mode: GuardrailMode::Off,
            validate_tool_calls: false,
            validate_json_mode: false,
            validate_schema: false,
            validate_steps: false,
            rescue_tool_calls: false,
            retry_with_nudge: false,
            streaming_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompactionConfig {
    pub enabled: bool,
    pub token_budget: TokenBudget,
    pub min_messages_to_keep: usize,
}

impl ContextCompactionConfig {
    pub fn budget(&self) -> ContextBudget {
        ContextBudget {
            max_context_tokens: self.token_budget.max_context_tokens,
            reserve_output_tokens: self.token_budget.reserve_output_tokens,
            compact_above_ratio: self.token_budget.compact_above_ratio,
            min_recent_messages: self.min_messages_to_keep,
        }
    }
}

impl Default for ContextCompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token_budget: TokenBudget::default(),
            min_messages_to_keep: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VramConfig {
    pub enabled: bool,
    pub static_max_context_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuardrailTraceConfig {
    pub enabled: bool,
    pub include_prompt_text: bool,
    pub include_tool_arguments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecoveryConfig {
    pub enabled: bool,
    pub retry_same_provider: bool,
    pub retry_fallback_provider: bool,
    pub retry_model_swap: bool,
}

impl GuardrailsConfig {
    pub fn route_config(&self, route: &RouteKind) -> &RouteGuardrailConfig {
        match route {
            RouteKind::ChatCompletions => &self.chat_completions,
            RouteKind::AnthropicMessages => &self.anthropic_messages,
            RouteKind::Responses => &self.responses,
        }
    }
}
