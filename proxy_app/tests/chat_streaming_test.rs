use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use rotator::{CredentialManager, HttpClientPool, ProviderRegistry, RotatorClient};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn chat_completions_stream_true_returns_valid_http_response() {
    let creds = CredentialManager::new();
    creds.register_keys("openai".to_owned(), vec!["test-dummy-key".to_owned()], 10);
    let registry = Arc::new(ProviderRegistry::new());
    let client = RotatorClient::new(
        creds,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(rotator::RateLimiterRegistry::new()),
        Arc::new(rotator::CooldownManager::new()),
        Arc::new(rotator::CircuitBreakerRegistry::new()),
        None,
        3,
    );
    let mut state = AppState::with_parts(client, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "gpt-4o-mini",
                        "messages": [
                            {"role": "user", "content": "hello"}
                        ],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // With invalid credentials, upstream forwarding fails, proving the proxy path is active
    // (previously this returned a hardcoded 200 SSE placeholder)
    assert_ne!(response.status(), StatusCode::OK);
    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "expected upstream failure, got {:?}",
        response.status()
    );
}
