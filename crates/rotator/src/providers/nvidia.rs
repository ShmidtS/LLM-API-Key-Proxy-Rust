use super::Provider;
use crate::error::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct NvidiaProvider {
    base_url: String,
}

impl NvidiaProvider {
    pub fn new() -> Self {
        Self {
            base_url: "https://integrate.api.nvidia.com/v1".to_owned(),
        }
    }
}

fn strip_unsupported_anthropic_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.remove("thinking");
            object.remove("cache_control");
            object.remove("thinking_signature");
            object.remove("betas");
            for value in object.values_mut() {
                strip_unsupported_anthropic_fields(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                strip_unsupported_anthropic_fields(value);
            }
        }
        _ => {}
    }
}

fn sanitize_tools(value: &mut serde_json::Value) {
    let Some(tools) = value.get_mut("tools").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for tool in tools {
        let Some(func) = tool.get_mut("function").and_then(|v| v.as_object_mut()) else {
            continue;
        };
        func.insert(
            "parameters".to_owned(),
            serde_json::json!({"type": "object"}),
        );
    }
}

fn sanitize_tool_choice(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(choice) = object.get("tool_choice")
        && choice.is_object()
    {
        object.insert("tool_choice".to_owned(), serde_json::json!("required"));
    }
}

fn remove_stream_options(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("stream_options");
}

fn remove_reasoning_effort(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("reasoning_effort");
}

fn strip_extra_body_chat_template_kwargs(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(extra_body) = object.get_mut("extra_body").and_then(|v| v.as_object_mut()) else {
        return;
    };
    extra_body.remove("chat_template_kwargs");
    if extra_body.is_empty() {
        object.remove("extra_body");
    }
}

#[async_trait]
impl Provider for NvidiaProvider {
    fn id(&self) -> &str {
        "nvidia"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn transform_request(&self, body: &mut serde_json::Value) {
        strip_unsupported_anthropic_fields(body);
        remove_stream_options(body);
        sanitize_tool_choice(body);
        strip_extra_body_chat_template_kwargs(body);
        sanitize_tools(body);
        remove_reasoning_effort(body);
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
    fn nvidia_provider_exposes_expected_metadata_and_auth() {
        let provider = NvidiaProvider::new();

        assert_eq!(provider.id(), "nvidia");
        assert_eq!(provider.base_url(), "https://integrate.api.nvidia.com/v1");
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![("authorization".to_owned(), "Bearer test-key".to_owned())]
        );
        assert!(provider.supports_streaming());
    }

    #[test]
    fn nvidia_transform_simplifies_tool_parameters() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "location": {"type": "string"}
                        },
                        "required": ["location"]
                    }
                }
            }]
        });

        NvidiaProvider::new().transform_request(&mut body);

        let func = &body["tools"][0]["function"];
        assert_eq!(func["parameters"], serde_json::json!({"type": "object"}));
        assert!(func["parameters"].get("properties").is_none());
        assert!(func["parameters"].get("required").is_none());
        assert_eq!(func["name"], "get_weather");
    }

    #[test]
    fn nvidia_transform_converts_dict_tool_choice_to_required() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}}
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn nvidia_transform_leaves_string_tool_choice_unchanged() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "tool_choice": "auto"
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn nvidia_transform_removes_stream_options() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "stream_options": {"include_usage": true}
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn nvidia_transform_removes_reasoning_effort() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "reasoning_effort": "high"
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn nvidia_transform_removes_betas() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "betas": ["computer-use-2025-01"]
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert!(body.get("betas").is_none());
    }

    #[test]
    fn nvidia_transform_removes_extra_body_chat_template_kwargs() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "extra_body": {
                "chat_template_kwargs": {"thinking": true}
            }
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert!(body.get("extra_body").is_none());
    }

    #[test]
    fn nvidia_transform_keeps_extra_body_with_other_fields() {
        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "extra_body": {
                "chat_template_kwargs": {"thinking": true},
                "other_field": 42
            }
        });

        NvidiaProvider::new().transform_request(&mut body);

        assert!(body.get("extra_body").is_some());
        assert!(body["extra_body"].get("chat_template_kwargs").is_none());
        assert_eq!(body["extra_body"]["other_field"], 42);
    }

    #[test]
    fn nvidia_transform_keeps_simple_request_intact() {
        let original = serde_json::json!({
            "model": "nvidia/llama-3.1",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        });
        let mut body = original.clone();

        NvidiaProvider::new().transform_request(&mut body);

        assert_eq!(body, original);
    }

    #[tokio::test]
    #[ignore = "requires live NVIDIA API key"]
    async fn nvidia_live_smoke_with_tools_returns_200() {
        let api_key = match std::env::var("NVIDIA_API_KEY_0") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skipping live smoke: NVIDIA_API_KEY_0 not set");
                return;
            }
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();

        let mut body = serde_json::json!({
            "model": "nvidia/moonshotai/kimi-k2.6",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "location": {"type": "string"}
                        },
                        "required": ["location"]
                    }
                }
            }],
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
            "stream": false,
            "max_tokens": 16
        });

        let provider = NvidiaProvider::new();
        provider.transform_request(&mut body);

        let response = provider
            .request(&client, "chat/completions", body, &api_key)
            .await
            .expect("request should succeed");

        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "expected 2xx, got {}. body: {}",
            status,
            body_text
        );
    }
}
