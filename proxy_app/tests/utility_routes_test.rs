use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use proxy_app::state::AppState;
use rotator::{
    CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool, ProviderRegistry,
    RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

static ADMIN_TOKEN_LOCK: Mutex<()> = Mutex::const_new(());

fn app_without_credentials() -> axum::Router {
    app_with_admin_token(Some("test-admin-token"))
}

fn app_with_admin_token(admin_token: Option<&str>) -> axum::Router {
    let credentials = CredentialManager::new();
    let registry = Arc::new(ProviderRegistry::new());
    let rotator = RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        3,
    );

    let mut state = AppState::with_parts(rotator, registry);
    state.config.admin_token = admin_token.map(str::to_owned);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    proxy_app::build_app_with_state(state)
}

async fn request_json(method: Method, uri: &str) -> (StatusCode, Value) {
    request_json_with_auth(method, uri, None).await
}

async fn request_json_with_auth(
    method: Method,
    uri: &str,
    authorization: Option<&str>,
) -> (StatusCode, Value) {
    request_json_with_auth_and_body(method, uri, authorization, "{}").await
}

async fn request_json_with_auth_and_body(
    method: Method,
    uri: &str,
    authorization: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    request_json_with_admin_token_and_body(
        method,
        uri,
        authorization,
        body,
        Some("test-admin-token"),
    )
    .await
}

async fn request_json_with_admin_token_and_body(
    method: Method,
    uri: &str,
    authorization: Option<&str>,
    body: &str,
    admin_token: Option<&str>,
) -> (StatusCode, Value) {
    let app = app_with_admin_token(admin_token);
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-api-key", "test-proxy-token");

    if let Some(authorization) = authorization {
        builder = builder.header("authorization", authorization);
    }

    let response = app
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();

    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

async fn request_multipart(uri: &str) -> (StatusCode, Value) {
    let app = app_without_credentials();
    let body = "------WebKitFormBoundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\nContent-Type: text/plain\r\n\r\ntest content\r\n------WebKitFormBoundary--\r\n";
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(
                    "content-type",
                    "multipart/form-data; boundary=----WebKitFormBoundary",
                )
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn test_files_multipart_forwarding() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }

    let (status, json) = request_multipart("/v1/files").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no credentials available for provider: openai")
    );
}

#[tokio::test]
async fn test_audio_transcriptions_multipart_forwarding() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }

    let (status, json) = request_multipart("/v1/audio/transcriptions").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no credentials available for provider: openai")
    );
}

#[tokio::test]
async fn test_files_routes_forward_to_upstream() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }

    let routes = [
        (Method::POST, "/v1/files"),
        (Method::GET, "/v1/files"),
        (Method::GET, "/v1/files/file_123"),
        (Method::DELETE, "/v1/files/file_123"),
    ];

    for (method, uri) in routes {
        let (status, json) = request_json(method, uri).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{uri}");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no credentials available for provider: openai"),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn test_batches_routes_forward_to_upstream() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }

    let routes = [
        (Method::POST, "/v1/batches"),
        (Method::GET, "/v1/batches"),
        (Method::GET, "/v1/batches/batch_123"),
        (Method::POST, "/v1/batches/batch_123/cancel"),
    ];

    for (method, uri) in routes {
        let (status, json) = request_json(method, uri).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{uri}");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no credentials available for provider: openai"),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn admin_stats_returns_provider_key_and_request_counts() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::set_var("ADMIN_TOKEN", "test-admin-token");
    }
    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec!["openai-test-key".to_owned()], 10);
    credentials.register_keys(
        "anthropic".to_owned(),
        vec!["anthropic-test-key".to_owned()],
        10,
    );
    let registry = Arc::new(ProviderRegistry::new());
    let rotator = RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        3,
    );

    let mut state = AppState::with_parts(rotator, registry);
    state.config.admin_token = Some("test-admin-token".to_owned());

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/stats")
                .header("authorization", "Bearer test-admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(json["total_keys"].as_u64().unwrap() >= 2);
    assert_eq!(json["active_requests"], 0);
    let providers = json["providers"].as_array().unwrap();
    let openai = providers
        .iter()
        .find(|provider| provider["id"] == "openai")
        .expect("openai provider exists");
    assert_eq!(openai["display_name"], "OpenAI");
    assert_eq!(
        openai["endpoints"],
        serde_json::json!(["/chat/completions", "/embeddings", "/images/generations"])
    );
    assert_eq!(
        openai["features"],
        serde_json::json!(["chat", "streaming", "embeddings", "vision", "images"])
    );
    assert!(openai["model_count"].as_u64().unwrap() > 0);
    assert!(
        providers
            .iter()
            .any(|provider| provider["id"] == "anthropic")
    );

    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }
}

#[tokio::test]
async fn admin_token_count_uses_bpe_chat_tokens() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::set_var("ADMIN_TOKEN", "test-admin-token");
    }

    let app = app_without_credentials();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/token_count")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-admin-token")
                .body(Body::from(
                    r#"{"model":"gpt-4o","messages":[{"role":"user","content":"12345678"},{"role":"assistant","content":"1234"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let expected = rotator::tokenizer::count_chat_tokens(
        &[
            json!({"role": "user", "content": "12345678"}),
            json!({"role": "assistant", "content": "1234"}),
        ],
        "gpt-4o",
    );

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["token_count"], expected);

    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }
}

#[tokio::test]
async fn admin_cost_estimate_uses_model_prices() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::set_var("ADMIN_TOKEN", "test-admin-token");
    }

    let app = app_without_credentials();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/cost_estimate")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-admin-token")
                .body(Body::from(
                    r#"{"model":"gpt-4o","input_tokens":1000000,"output_tokens":1000000}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["estimated_cost_usd"], 12500.0);

    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }
}

#[tokio::test]
async fn tools_routes_attempt_forwarding() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::set_var("ADMIN_TOKEN", "test-admin-token");
    }

    let (status, json) = request_json_with_auth(
        Method::POST,
        "/v1/tools/web-search",
        Some("Bearer test-admin-token"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "/v1/tools/web-search");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("query or input"),
        "/v1/tools/web-search"
    );

    let routes = [
        (Method::POST, "/v1/tools/tokenizer"),
        (Method::POST, "/v1/tools/layout-parsing"),
        (Method::POST, "/v1/tools/web-reader"),
    ];

    for (method, uri) in routes {
        let (status, json) =
            request_json_with_auth(method, uri, Some("Bearer test-admin-token")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{uri}");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no credentials available for provider: zai"),
            "{uri}"
        );
    }

    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }
}

#[tokio::test]
async fn agents_routes_attempt_forwarding() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::set_var("ADMIN_TOKEN", "test-admin-token");
    }

    let routes = [
        (Method::POST, "/v1/agents/chat"),
        (Method::POST, "/v1/agents/file-upload"),
        (Method::GET, "/v1/agents/async-result?id=result_123"),
        (Method::POST, "/v1/agents/conversation"),
    ];

    for (method, uri) in routes {
        let (status, json) =
            request_json_with_auth(method, uri, Some("Bearer test-admin-token")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{uri}");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no credentials available for provider: zai"),
            "{uri}"
        );
    }

    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }
}

#[tokio::test]
async fn protected_routes_deny_access_when_admin_token_is_unset() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }

    let routes = [
        (Method::GET, "/v1/quota-stats"),
        (Method::POST, "/v1/tools/web-search"),
        (Method::POST, "/v1/agents/chat"),
    ];

    for (method, uri) in routes {
        let (status, _) = request_json_with_admin_token_and_body(
            method,
            uri,
            Some("Bearer test-admin-token"),
            "{}",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn protected_routes_require_bearer_token_when_admin_token_is_configured() {
    let _guard = ADMIN_TOKEN_LOCK.lock().await;
    unsafe {
        std::env::set_var("ADMIN_TOKEN", "secret-token");
    }

    let routes = [
        (Method::GET, "/v1/quota-stats"),
        (Method::POST, "/v1/tools/web-search"),
        (Method::POST, "/v1/agents/chat"),
    ];

    for (method, uri) in routes {
        let (status, _) = request_json_with_admin_token_and_body(
            method.clone(),
            uri,
            None,
            "{}",
            Some("secret-token"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");

        let (status, _) = request_json_with_admin_token_and_body(
            method.clone(),
            uri,
            Some("Bearer wrong-token"),
            "{}",
            Some("secret-token"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");

        let (status, _) = request_json_with_admin_token_and_body(
            method,
            uri,
            Some("Bearer secret-token"),
            "{}",
            Some("secret-token"),
        )
        .await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }

    unsafe {
        std::env::remove_var("ADMIN_TOKEN");
    }
}
