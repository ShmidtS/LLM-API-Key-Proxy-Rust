use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    pub total_tokens: u32,
}
