use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use rotator::{
    AuthType, CredentialManager, HttpClientPool, ProviderDefinition, ProviderRegistry,
    RotatorClient,
};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

async fn passthrough_server(
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = [0; 4096];
            let _ = socket.read(&mut buffer).await;
            let response = format!(
                "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\n\r\n{}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK"),
                content_type,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    format!("http://{addr}/v1")
}

fn test_state(provider: &str, base_url: String, model_pattern: &str) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: provider.to_owned(),
        display_name: provider.to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![model_pattern.to_owned()],
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    let credentials = CredentialManager::new();
    credentials.register_keys(provider.to_owned(), vec!["test-key".to_owned()], 10);
    let rotator = RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(rotator::RateLimiterRegistry::new()),
        Arc::new(rotator::CooldownManager::new()),
        Arc::new(rotator::CircuitBreakerRegistry::new()),
        None,
        0,
    );
    let mut state = AppState::with_parts(rotator, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    state
}

#[tokio::test]
async fn chat_non_streaming_returns_upstream_status_content_type_and_body_as_is() {
    let upstream_body = r#"{"custom":"shape","choices":[]}"#;
    let state = test_state(
        "openai",
        passthrough_server(
            StatusCode::ACCEPTED,
            "application/vnd.test+json",
            upstream_body,
        )
        .await,
        r"^gpt-.*",
    );

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
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.test+json"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), upstream_body.as_bytes());
}

#[tokio::test]
async fn embeddings_returns_upstream_status_content_type_and_body_as_is() {
    let upstream_body = r#"{"embedding_provider":"openai","data":[]}"#;
    let state = test_state(
        "openai",
        passthrough_server(StatusCode::CREATED, "application/x-ndjson", upstream_body).await,
        r"^text-embedding-3-.*",
    );

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/embeddings")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "text-embedding-3-small",
                        "input": "hello"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/x-ndjson"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), upstream_body.as_bytes());
}
