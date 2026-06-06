use super::Provider;
use crate::error::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct AnthropicProvider {
    base_url: String,
}

impl AnthropicProvider {
    pub fn new() -> Self {
        Self {
            base_url: "https://api.anthropic.com/v1".to_owned(),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("authorization".to_owned(), format!("Bearer {api_key}")),
            ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
        ]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn transform_request(&self, body: &mut serde_json::Value) {
        let Some(object) = body.as_object_mut() else {
            return;
        };
        object.remove("stream_options");

        // Claude 4 adaptive thinking conversion
        let model_value = object
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let model_name = model_value
            .split_once('/')
            .map(|(_, m)| m)
            .unwrap_or(model_value);
        if !(model_name.starts_with("claude-opus-4") || model_name.starts_with("claude-sonnet-4")) {
            return;
        }
        let Some(effort) = object.get("reasoning_effort").and_then(|v| v.as_str()) else {
            return;
        };
        let effort_str = effort.to_lowercase();
        if !matches!(effort_str.as_str(), "low" | "medium" | "high") {
            return;
        }
        if object.get("thinking").is_some() || object.get("output_config").is_some() {
            return;
        }
        object.remove("reasoning_effort");
        object.insert(
            "thinking".to_owned(),
            serde_json::json!({"type": "adaptive"}),
        );
        object.insert(
            "output_config".to_owned(),
            serde_json::json!({"effort": effort_str}),
        );
    }

    async fn request(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: serde_json::Value,
        api_key: &str,
    ) -> Result<reqwest::Response> {
        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut request = client.post(url).json(&body);

        for (name, value) in self.auth_headers(api_key) {
            request = request.header(name, value);
        }

        Ok(request.send().await?)
    }

    async fn list_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut request = client.get(url);

        for (name, value) in self.auth_headers(api_key) {
            request = request.header(name, value);
        }

        let value: serde_json::Value = request.send().await?.json().await?;
        Ok(value
            .get("data")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_provider_exposes_expected_metadata_and_auth() {
        let provider = AnthropicProvider::new();

        assert_eq!(provider.id(), "anthropic");
        assert_eq!(provider.base_url(), "https://api.anthropic.com/v1");
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![
                ("authorization".to_owned(), "Bearer test-key".to_owned()),
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
            ]
        );
        assert!(provider.supports_streaming());
    }

    #[test]
    fn claude_opus_4_converts_reasoning_effort_to_adaptive_thinking() {
        let provider = AnthropicProvider::new();
        let mut body = serde_json::json!({
            "model": "claude-opus-4-1-20250805",
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hello"}],
        });
        provider.transform_request(&mut body);

        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(body["output_config"], serde_json::json!({"effort": "high"}));
    }

    #[test]
    fn claude_sonnet_4_converts_reasoning_effort_to_adaptive_thinking() {
        let provider = AnthropicProvider::new();
        let mut body = serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "reasoning_effort": "medium",
        });
        provider.transform_request(&mut body);

        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(
            body["output_config"],
            serde_json::json!({"effort": "medium"})
        );
    }

    #[test]
    fn non_claude_4_reasoning_effort_left_unchanged() {
        let provider = AnthropicProvider::new();
        let mut body = serde_json::json!({
            "model": "claude-3-7-sonnet-20250219",
            "reasoning_effort": "low",
        });
        provider.transform_request(&mut body);

        assert_eq!(body["reasoning_effort"], "low");
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn claude_4_existing_thinking_not_overridden() {
        let provider = AnthropicProvider::new();
        let mut body = serde_json::json!({
            "model": "claude-opus-4-1-20250805",
            "reasoning_effort": "high",
            "thinking": {"type": "enabled"},
        });
        provider.transform_request(&mut body);

        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn stream_options_still_removed_for_all_models() {
        let provider = AnthropicProvider::new();
        let mut body = serde_json::json!({
            "model": "claude-opus-4-1-20250805",
            "stream_options": {"include_usage": true},
        });
        provider.transform_request(&mut body);

        assert!(body.get("stream_options").is_none());
    }
}
