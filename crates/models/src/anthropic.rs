use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicTextBlock {
    pub r#type: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicImageSource {
    pub r#type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicImageBlock {
    pub r#type: String,
    pub source: AnthropicImageSource,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicToolUseBlock {
    pub r#type: String,
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicToolResultBlock {
    pub r#type: String,
    pub tool_use_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text(AnthropicTextBlock),
    #[serde(rename = "image")]
    Image(AnthropicImageBlock),
    #[serde(rename = "tool_use")]
    ToolUse(AnthropicToolUseBlock),
    #[serde(rename = "tool_result")]
    ToolResult(AnthropicToolResultBlock),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicThinkingConfig {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinkingConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicCountTokensRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
}
