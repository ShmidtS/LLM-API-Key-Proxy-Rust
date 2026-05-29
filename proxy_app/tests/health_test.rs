use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn empty_app() -> axum::Router {
    let creds = rotator::CredentialManager::new();
    let pool = rotator::HttpClientPool::new(30);
    let registry = std::sync::Arc::new(rotator::ProviderRegistry::new());
    let client = rotator::RotatorClient::new(
        creds,
        pool,
        registry.clone(),
        std::sync::Arc::new(rotator::RateLimiterRegistry::new()),
        std::sync::Arc::new(rotator::CooldownManager::new()),
        std::sync::Arc::new(rotator::CircuitBreakerRegistry::new()),
        None,
        3,
    );
    let mut state = proxy_app::state::AppState::with_parts(client, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    proxy_app::build_app_with_state(state)
}

#[tokio::test]
async fn test_health_endpoint() {
    let app = empty_app();
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn root_head_returns_ok_with_empty_body() {
    let response = empty_app()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(bytes.is_empty());
}

async fn post_json(uri: &str, body: Value) -> axum::response::Response {
    empty_app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_chat_model_routes_to_provider(model: &str, provider: &str) {
    let response = post_json(
        "/v1/chat/completions",
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    let body = response_json(response).await;

    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&format!(
                "no credentials available for provider: {provider}"
            )),
        "model {model} did not resolve to provider {provider}: {body}"
    );
}

#[tokio::test]
async fn chat_provider_resolution_uses_regex_fallback_before_openai_default() {
    for (model, provider) in [
        ("gemini/gemini-2.5-flash", "gemini"),
        ("anthropic/claude-3-5-sonnet-20241022", "anthropic"),
        ("openai/gpt-4o", "openai"),
        ("openrouter/openai/gpt-4o", "openrouter"),
        ("unknown-model", "openai"),
    ] {
        assert_chat_model_routes_to_provider(model, provider).await;
    }
}

#[tokio::test]
async fn props_endpoints_return_parity_schema() {
    for uri in ["/v1/props", "/props"] {
        let response = empty_app()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            response_json(response).await,
            json!({"version": "1.16", "mode": "llm", "gpu_devices": []})
        );
    }
}

#[tokio::test]
async fn version_endpoint_returns_parity_version() {
    let response = empty_app()
        .oneshot(
            Request::builder()
                .uri("/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response_json(response).await, json!({"version": "1.16"}));
}

#[tokio::test]
async fn embeddings_endpoint_forwards_to_upstream() {
    let response = post_json(
        "/v1/embeddings",
        json!({
            "model": "text-embedding-3-small",
            "input": "hello"
        }),
    )
    .await;

    assert_eq!(response.status(), 502);
    let body = response_json(response).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no credentials available for provider: openai")
    );
}

#[tokio::test]
async fn anthropic_messages_endpoint_forwards_to_upstream() {
    let response = post_json(
        "/v1/messages",
        json!({
            "model": "claude-3-5-sonnet-latest",
            "max_tokens": 16,
            "messages": []
        }),
    )
    .await;

    assert_eq!(response.status(), 502);
    let body = response_json(response).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no credentials available for provider: anthropic")
    );
}

#[tokio::test]
async fn anthropic_count_tokens_endpoint_forwards_to_upstream() {
    let response = post_json(
        "/v1/messages/count_tokens",
        json!({
            "model": "claude-3-5-sonnet-latest",
            "messages": []
        }),
    )
    .await;

    assert_eq!(response.status(), 502);
    let body = response_json(response).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no credentials available for provider: anthropic")
    );
}
