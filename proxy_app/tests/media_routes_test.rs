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
    time::{Duration, timeout},
};
use tower::ServiceExt;

fn authed_app() -> axum::Router {
    let mut state = AppState::new();
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    proxy_app::build_app_with_state(state)
}

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

fn media_test_state(base_url: String) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: "openai".to_owned(),
        display_name: "openai".to_owned(),
        base_url: base_url.clone(),
        auth_type: AuthType::ApiKey,
        model_patterns: vec![r"^gpt-.*".to_owned()],
        endpoints: vec!["/images".to_owned()],
        features: vec!["images".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });
    registry.register(ProviderDefinition {
        id: "zai".to_owned(),
        display_name: "zai".to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![r"^glm-.*".to_owned()],
        endpoints: vec!["/video".to_owned()],
        features: vec!["video".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec!["test-key".to_owned()], 10);
    credentials.register_keys("zai".to_owned(), vec!["test-key".to_owned()], 10);
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
async fn test_media_post_routes_forward_to_upstream() {
    let routes = [
        "/v1/audio/speech",
        "/v1/audio/transcriptions",
        "/v1/images/generations",
        "/v1/images/edits",
        "/v1/images/variations",
        "/v1/moderations",
    ];

    for route in routes {
        let app = authed_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(route)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::from(
                        json!({"model": "dall-e-3", "input": "hello"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            502,
            "route {route} did not attempt upstream proxy"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body)
            .unwrap_or_else(|err| panic!("route {route} did not return valid JSON: {err}"));

        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no credentials available for provider: openai"),
            "route {route} did not return upstream credential error"
        );
    }
}

#[tokio::test]
async fn test_image_get_routes_forward_to_upstream() {
    let routes = [
        ("/images/img_123", "GET /v1/images/img_123"),
        ("/v1/images/img_456", "GET /v1/images/img_456"),
    ];

    for (route, expected_request_line_prefix) in routes {
        let (base_url, request_rx) = capture_server().await;
        let state = media_test_state(base_url);

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(route)
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");

        let request = timeout(Duration::from_secs(2), request_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            request
                .lines()
                .next()
                .unwrap()
                .starts_with(expected_request_line_prefix),
            "{route}"
        );
    }
}

#[tokio::test]
async fn test_video_get_routes_forward_to_upstream() {
    let routes = [
        ("/video/status?id=vid_123", "GET /v1/video/status"),
        ("/v1/video/status?id=vid_456", "GET /v1/video/status"),
        ("/v1/video/vid_789/status", "GET /v1/video/vid_789/status"),
    ];

    for (route, expected_request_line_prefix) in routes {
        let (base_url, request_rx) = capture_server().await;
        let state = media_test_state(base_url);

        let response = proxy_app::build_app_with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(route)
                    .header("x-api-key", "test-proxy-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");

        let request = timeout(Duration::from_secs(2), request_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            request
                .lines()
                .next()
                .unwrap()
                .starts_with(expected_request_line_prefix),
            "{route}"
        );
    }
}
