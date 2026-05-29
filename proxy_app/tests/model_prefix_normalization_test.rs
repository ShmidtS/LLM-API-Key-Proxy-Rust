use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

async fn upstream_server(captured_request: Arc<Mutex<String>>, body: Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0; 8192];
        let bytes_read = socket.read(&mut buffer).await.unwrap_or(0);
        *captured_request.lock().unwrap() =
            String::from_utf8_lossy(&buffer[..bytes_read]).to_string();

        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });

    format!("http://{addr}/v1")
}

fn test_state(provider: &str, base_url: String) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: provider.to_owned(),
        display_name: provider.to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![format!(r"^{provider}/.*")],
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

#[tokio::test]
async fn chat_completions_preserves_openai_provider_prefix_before_upstream() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let upstream_body = json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 123,
        "model": "openai/gpt-5.5",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    });
    let state = test_state(
        "openai",
        upstream_server(captured_request.clone(), upstream_body).await,
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
                        "model": "openai/gpt-5.5",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let raw_request = captured_request.lock().unwrap().clone();
    let (_, body) = raw_request.split_once("\r\n\r\n").unwrap();
    let upstream_json: Value = serde_json::from_str(body).unwrap();
    assert_eq!(upstream_json["model"], "openai/gpt-5.5");
}

#[tokio::test]
async fn chat_completions_strips_non_openai_provider_prefix_before_upstream() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let upstream_body = json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-3-5-sonnet",
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let state = test_state(
        "anthropic",
        upstream_server(captured_request.clone(), upstream_body).await,
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
                        "model": "anthropic/claude-3-5-sonnet",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let raw_request = captured_request.lock().unwrap().clone();
    let (_, body) = raw_request.split_once("\r\n\r\n").unwrap();
    let upstream_json: Value = serde_json::from_str(body).unwrap();
    assert_eq!(upstream_json["model"], "claude-3-5-sonnet");
}
