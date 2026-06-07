use super::Provider;
use crate::error::Result;
use crate::providers::gemini_tool_handler;
use crate::providers::openai::OpenAiProvider;
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct GeminiProvider {
    inner: OpenAiProvider,
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self::new_with_base_url("https://generativelanguage.googleapis.com/v1beta")
    }

    pub fn new_with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            inner: OpenAiProvider::new(base_url),
        }
    }

    /// Group Gemini-format tool responses with their matching function calls.
    pub fn group_tool_responses(&self, contents: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
        gemini_tool_handler::group_tool_responses(&contents)
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &str {
        "gemini"
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        self.inner.auth_headers(api_key)
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn transform_request(&self, body: &mut serde_json::Value) {
        if let Some(model) = body.get("model").and_then(|value| value.as_str()) {
            let model = model.strip_prefix("gemini/").unwrap_or(model);
            if !model.starts_with("models/") {
                body["model"] = serde_json::Value::String(format!("models/{model}"));
            } else {
                body["model"] = serde_json::Value::String(model.to_owned());
            }
        }

        if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
            let tool_defs: Vec<models::chat::ToolDefinition> = tools
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            if !tool_defs.is_empty() {
                let gemini_tools = gemini_tool_handler::transform_tools_to_gemini(&tool_defs);
                if let Ok(val) = serde_json::to_value(gemini_tools) {
                    body["tools"] = val;
                }
            }
        }

        if let Some(tool_choice) = body.get("tool_choice")
            && let Ok(tc) = serde_json::from_value::<models::chat::ToolChoice>(tool_choice.clone())
            && let Some(config) = gemini_tool_handler::transform_tool_choice_to_gemini(&tc)
            && let Ok(val) = serde_json::to_value(config)
        {
            body["toolConfig"] = val;
        }
    }

    async fn request(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: serde_json::Value,
        api_key: &str,
    ) -> Result<reqwest::Response> {
        self.inner.request(client, path, body, api_key).await
    }

    async fn list_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        self.inner.list_models(client, api_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_provider_exposes_expected_metadata_and_auth() {
        let provider = GeminiProvider::new();

        assert_eq!(provider.id(), "gemini");
        assert_eq!(
            provider.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![("authorization".to_owned(), "Bearer test-key".to_owned())]
        );
        assert!(provider.supports_streaming());
    }
}
