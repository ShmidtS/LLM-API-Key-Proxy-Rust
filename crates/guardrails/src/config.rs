use crate::types::{GuardrailMode, TokenBudget};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuardrailsConfig {
    pub chat_completions: RouteGuardrailConfig,
    pub anthropic_messages: RouteGuardrailConfig,
    pub responses: RouteGuardrailConfig,
    pub max_rescue_attempts: u32,
    pub max_guardrail_retries: u32,
    pub context_compaction: ContextCompactionConfig,
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompactionConfig {
    pub enabled: bool,
    pub token_budget: TokenBudget,
    pub min_messages_to_keep: usize,
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
pub struct RecoveryConfig {
    pub enabled: bool,
    pub retry_same_provider: bool,
    pub retry_fallback_provider: bool,
    pub retry_model_swap: bool,
}

impl GuardrailsConfig {
    pub fn route_config(&self, route: &crate::types::RouteKind) -> &RouteGuardrailConfig {
        match route {
            crate::types::RouteKind::ChatCompletions => &self.chat_completions,
            crate::types::RouteKind::AnthropicMessages => &self.anthropic_messages,
            crate::types::RouteKind::Responses => &self.responses,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RouteKind;

    #[test]
    fn defaults_disable_all_guardrails() {
        let config = GuardrailsConfig::default();
        assert_eq!(config.chat_completions.mode, GuardrailMode::Off);
        assert!(!config.context_compaction.enabled);
        assert_eq!(config.max_guardrail_retries, 0);
        assert!(!config.recovery.enabled);
    }

    #[test]
    fn selects_route_config() {
        let config = GuardrailsConfig::default();
        assert_eq!(
            config.route_config(&RouteKind::Responses).mode,
            GuardrailMode::Off
        );
    }
}
