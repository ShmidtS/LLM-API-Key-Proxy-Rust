use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tower::ServiceExt;

async fn capture_server() -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0; 4096];
        let n = socket.read(&mut buffer).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..n]).to_string();
        let _ = tx.send(request);
        let response =
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        let _ = socket.write_all(response.as_bytes()).await;
    });

    (format!("http://{addr}/v1"), rx)
}

fn provider_definition(id: &str, base_url: String, model_pattern: &str) -> ProviderDefinition {
    ProviderDefinition {
        id: id.to_owned(),
        display_name: id.to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![model_pattern.to_owned()],
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
    }
}

fn test_state(provider: &str, base_url: String, model_pattern: &str) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(provider_definition(provider, base_url, model_pattern));

    let credentials = CredentialManager::new();
    credentials.register_keys(provider.to_owned(), vec!["test-key".to_owned()], 10);
    let rotator = RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        0,
    );
    let mut state = AppState::with_parts(rotator, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    state
}

fn multi_provider_state() -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(provider_definition(
        "openai",
        "http://127.0.0.1:1/v1".to_owned(),
        r"^gpt-.*",
    ));
    registry.register(provider_definition(
        "anthropic",
        "http://127.0.0.1:1/v1".to_owned(),
        r"^claude-.*",
    ));

    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec!["openai-test-key".to_owned()], 10);
    credentials.register_keys(
        "anthropic".to_owned(),
        vec!["anthropic-test-key".to_owned()],
        10,
    );
    let rotator = RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        0,
    );
    let mut state = AppState::with_parts(rotator, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    state.config.admin_token = Some("test-admin-token".to_owned());
    state
}

mod openai_routes_parity {
    use super::*;

    #[tokio::test]
    async fn files_list_forwards_query_params() {
        let (base_url, request_rx) = capture_server().await;
        let state = test_state("openai", base_url, r"^gpt-.*");

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/files?limit=10")
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let request = request_rx.await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            request
                .lines()
                .next()
                .unwrap()
                .contains("/v1/files?limit=10")
        );
    }

    #[tokio::test]
    async fn batches_list_forwards_query_params() {
        let (base_url, request_rx) = capture_server().await;
        let state = test_state("openai", base_url, r"^gpt-.*");

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/batches?limit=5")
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let request = request_rx.await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            request
                .lines()
                .next()
                .unwrap()
                .contains("/v1/batches?limit=5")
        );
    }

    #[tokio::test]
    async fn files_delete_reaches_upstream() {
        let (base_url, request_rx) = capture_server().await;
        let state = test_state("openai", base_url, r"^gpt-.*");

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/v1/files/file_123")
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let request = request_rx.await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            request
                .lines()
                .next()
                .unwrap()
                .starts_with("DELETE /v1/files/file_123")
        );
    }

    #[tokio::test]
    async fn model_detail_with_slashes_matches_wildcard() {
        let state = multi_provider_state();

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/models/openai/gpt-4o")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["id"], "openai/gpt-4o");
    }

    #[tokio::test]
    async fn quota_stats_post_filters_by_provider() {
        let state = multi_provider_state();

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/quota-stats")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("authorization", "Bearer test-admin-token")
                    .body(Body::from(json!({"provider":"openai"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let providers = json["providers"].as_array().unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0]["id"], "openai");
    }
}
