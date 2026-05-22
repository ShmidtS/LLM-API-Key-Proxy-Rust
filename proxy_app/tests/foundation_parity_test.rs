use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use proxy_app::state::AppState;
use proxy_config::ProxyConfig;
use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

fn app_with_config(config: ProxyConfig) -> axum::Router {
    proxy_app::build_app_with_state(AppState::from_config(config))
}

fn auth_config() -> ProxyConfig {
    ProxyConfig {
        admin_token: Some("test-admin".to_owned()),
        api_keys: vec!["test-key".to_owned()],
        ..Default::default()
    }
}

async fn sse_upstream_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer).await;
        let response = concat!(
            "HTTP/1.1 200 OK\r\n",
            "content-type: text/event-stream\r\n",
            "connection: close\r\n",
            "\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });

    format!("http://{addr}/v1")
}

fn streaming_test_state(base_url: String) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: "openai".to_owned(),
        display_name: "openai".to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![r"^gpt-.*".to_owned()],
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
    credentials.register_keys("openai".to_owned(), vec!["openai-test-key".to_owned()], 10);
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
    state.config.api_keys = vec!["test-key".to_owned()];
    state
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();

    (status, body)
}

#[tokio::test]
async fn foundation_parity_bearer_auth_works_for_admin_routes() {
    let response = app_with_config(auth_config())
        .oneshot(
            Request::builder()
                .uri("/v1/quota-stats")
                .header(header::AUTHORIZATION, "Bearer test-admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn foundation_parity_x_api_key_works_for_proxy_routes() {
    let response = app_with_config(auth_config())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-key")
                .body(Body::from(
                    json!({
                        "model": "gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn foundation_parity_invalid_key_returns_openai_compatible_json_error() {
    let response = app_with_config(auth_config())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "bad-key")
                .body(Body::from(
                    json!({
                        "model": "gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let (status, body) = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(body["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn foundation_parity_cors_preflight_with_configured_origin_succeeds() {
    let mut config = auth_config();
    config.cors_allowed_origins = vec!["http://localhost:3000".to_owned()];
    config.cors_allowed_methods = vec!["POST".to_owned()];
    config.cors_allowed_headers = vec!["content-type".to_owned()];
    let response = app_with_config(config)
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/chat/completions")
                .header(header::ORIGIN, "http://localhost:3000")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_success());
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "http://localhost:3000"
    );
}

#[tokio::test]
async fn foundation_parity_cors_preflight_without_configured_origin_has_no_cors_header() {
    let response = app_with_config(auth_config())
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/chat/completions")
                .header(header::ORIGIN, "http://localhost:3000")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[tokio::test]
async fn foundation_parity_cors_exposes_proxy_headers() {
    let mut config = auth_config();
    config.cors_allowed_origins = vec!["http://localhost:3000".to_owned()];
    let response = app_with_config(config)
        .oneshot(
            Request::builder()
                .uri("/version")
                .header(header::ORIGIN, "http://localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let expose_headers = response
        .headers()
        .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .unwrap()
        .to_str()
        .unwrap();
    for expected in [
        "x-accel-buffering",
        "x-request-id",
        "x-provider",
        "retry-after",
        "x-ratelimit-limit",
        "x-ratelimit-remaining",
    ] {
        assert!(expose_headers.contains(expected), "{expected}");
    }
}

#[tokio::test]
async fn foundation_parity_sse_streaming_route_is_not_gzip_encoded() {
    let state = streaming_test_state(sse_upstream_server().await);
    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT_ENCODING, "gzip")
                .header("x-api-key", "test-key")
                .body(Body::from(
                    json!({
                        "model": "gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
}

#[tokio::test]
async fn foundation_parity_request_id_header_is_present() {
    let response = app_with_config(ProxyConfig::default())
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let request_id = response.headers().get("x-request-id").unwrap();
    assert!(!request_id.to_str().unwrap().is_empty());
}

#[tokio::test]
async fn foundation_parity_body_over_max_body_bytes_returns_openai_compatible_413_error() {
    let mut config = auth_config();
    config.max_body_bytes = 100;
    let large_body = json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "x".repeat(200)}]
    })
    .to_string();

    let response = app_with_config(config)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_LENGTH, large_body.len().to_string())
                .header("x-api-key", "test-key")
                .body(Body::from(large_body))
                .unwrap(),
        )
        .await
        .unwrap();

    let (status, body) = response_json(response).await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(body["error"]["type"], "invalid_request_error");
}
