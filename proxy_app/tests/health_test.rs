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
