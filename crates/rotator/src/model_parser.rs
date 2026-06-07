use reqwest::{StatusCode, header::HeaderMap};
use serde_json::Value;

pub fn parse_model_ids(provider: &str, value: Value) -> Vec<String> {
    let models = value
        .get("data")
        .or_else(|| value.get("models"))
        .unwrap_or(&value);

    models
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(model_id)
        .inspect(|id| {
            if id.is_empty() {
                tracing::debug!(provider, "skipping empty provider model id");
            }
        })
        .filter(|id| !id.is_empty())
        .collect()
}

fn model_id(value: &Value) -> Option<String> {
    if let Some(id) = value.as_str() {
        return Some(id.to_owned());
    }

    value
        .get("id")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub async fn parse_model_ids_response(
    provider: &str,
    response: reqwest::Response,
) -> Option<Vec<String>> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(provider, error = %error, "failed to read provider models response");
            return None;
        }
    };

    parse_model_ids_body(provider, status, &headers, &body)
}

pub fn parse_model_ids_body(
    provider: &str,
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
) -> Option<Vec<String>> {
    if !status.is_success() {
        log_model_response(
            provider,
            status,
            headers,
            body,
            "provider models request failed",
        );
        return None;
    }

    match serde_json::from_str::<Value>(body) {
        Ok(value) => Some(parse_model_ids(provider, value)),
        Err(error) => {
            let preview = body.chars().take(200).collect::<String>();
            let content_type = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("unknown");
            tracing::warn!(
                provider,
                status = %status,
                content_type,
                body_preview = %preview,
                error = %error,
                "failed to decode provider models"
            );
            None
        }
    }
}

fn log_model_response(
    provider: &str,
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
    message: &str,
) {
    let preview = body.chars().take(200).collect::<String>();
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");

    tracing::warn!(
        provider,
        status = %status,
        content_type,
        body_preview = %preview,
        message
    );
}

pub fn is_image_only_model(model: &str) -> bool {
    let model_name = model.split('/').next_back().unwrap_or(model).to_lowercase();
    matches!(
        model_name.as_str(),
        "dall-e-2" | "dall-e-3" | "dall-e-3-edit"
    ) || model_name.starts_with("flux")
        || model_name.starts_with("stable-diffusion")
        || model_name.starts_with("kandinsky")
        || model_name.starts_with("playground")
        || model_name.starts_with("imagen")
        || model_name.starts_with("ideogram")
        || model_name.starts_with("recraft")
        || model_name.starts_with("midjourney")
        || model_name.starts_with("art")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_image_only_model() {
        assert!(is_image_only_model("dall-e-3"));
        assert!(is_image_only_model("dall-e-2"));
        assert!(is_image_only_model("openai/dall-e-3"));
        assert!(is_image_only_model("flux-dev"));
        assert!(is_image_only_model("flux-schnell"));
        assert!(is_image_only_model("stable-diffusion-xl"));
        assert!(is_image_only_model("stable-diffusion-3"));
        assert!(is_image_only_model("kandinsky-2"));
        assert!(is_image_only_model("playground-v2.5"));
        assert!(is_image_only_model("imagen-3"));
        assert!(is_image_only_model("ideogram-v2"));
        assert!(is_image_only_model("recraft-v3"));
        assert!(is_image_only_model("midjourney-v6"));
        assert!(is_image_only_model("artistic-model"));
    }

    #[test]
    fn test_is_not_image_only_model() {
        assert!(!is_image_only_model("gpt-4"));
        assert!(!is_image_only_model("gpt-4o"));
        assert!(!is_image_only_model("claude-3-opus"));
        assert!(!is_image_only_model("openai/gpt-4"));
        assert!(!is_image_only_model(""));
        assert!(!is_image_only_model("dall-e"));
    }
}
