use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client_for(server: &MockServer, provider: &str, patterns: Vec<String>) -> RotatorClient {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: provider.to_owned(),
        display_name: provider.to_owned(),
        base_url: format!("{}/v1", server.uri()),
        auth_type: AuthType::Bearer,
        model_patterns: patterns,
        compiled_patterns: vec![],
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 60,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    let credentials = CredentialManager::new();
    credentials.register_keys(provider.to_owned(), vec!["test-key".to_owned()], 1);

    RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry,
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        0,
    )
}

async fn upstream_model(provider: &str, patterns: Vec<String>, model: &str) -> Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server, provider, patterns).await;
    client
        .request(
            provider,
            "chat/completions",
            json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    serde_json::from_slice(&requests[0].body).unwrap()
}

#[tokio::test]
async fn openrouter_strips_leading_prefix_but_keeps_inner_slash() {
    let body = upstream_model(
        "openrouter",
        vec![r"^openrouter/.*".to_owned()],
        "openrouter/anthropic/claude-3.5-sonnet",
    )
    .await;
    // Ведущий "openrouter/" снимается; внутренний namespace модели сохраняется.
    assert_eq!(body["model"], "anthropic/claude-3.5-sonnet");
}

#[tokio::test]
async fn xai_strips_provider_prefix() {
    let body = upstream_model("xai", vec![r"^xai/.*".to_owned()], "xai/grok-2").await;
    assert_eq!(body["model"], "grok-2");
}

#[tokio::test]
async fn kilocode_strips_provider_prefix() {
    let body = upstream_model(
        "kilocode",
        vec![r"^kilocode/.*".to_owned()],
        "kilocode/some-model",
    )
    .await;
    assert_eq!(body["model"], "some-model");
}

#[tokio::test]
async fn bare_model_without_prefix_is_unchanged() {
    let body = upstream_model("xai", vec![r"^xai/.*".to_owned()], "grok-2").await;
    assert_eq!(body["model"], "grok-2");
}

#[tokio::test]
async fn fireworks_strips_provider_prefix_and_passes_fireworks_model_name() {
    // Divergence from Python/LiteLLM: Rust talks directly to Fireworks API
    // (base_url https://api.fireworks.ai/inference/v1), so the LiteLLM-internal
    // "fireworks_ai/" alias is NOT needed. The upstream expects bare model names
    // such as "accounts/fireworks/models/llama-v3p1-405b-instruct".
    let body = upstream_model(
        "fireworks",
        vec![
            r"^accounts/fireworks/models/.*".to_owned(),
            r"^fireworks/.*".to_owned(),
        ],
        "fireworks/accounts/fireworks/models/llama-v3p1-405b-instruct",
    )
    .await;
    assert_eq!(
        body["model"],
        "accounts/fireworks/models/llama-v3p1-405b-instruct"
    );
}
