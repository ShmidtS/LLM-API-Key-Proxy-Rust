use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use reqwest::StatusCode;
use rotator::{
    AuthType, CredentialManager, HttpClientPool, ProviderDefinition, ProviderRegistry,
    RotatorClient,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

async fn model_server(status: StatusCode, body: &'static str) -> (String, Arc<AtomicUsize>) {
    delayed_model_server(status, body, Duration::ZERO).await
}

async fn delayed_model_server(
    status: StatusCode,
    body: &'static str,
    delay: Duration,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            count_clone.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0; 1024];
            let _ = socket.read(&mut buffer).await;
            tokio::time::sleep(delay).await;
            let response = format!(
                "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK"),
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    (format!("http://{addr}/v1"), count)
}

fn test_state(providers: Vec<(&str, String)>) -> proxy_app::state::AppState {
    let registry = Arc::new(ProviderRegistry::default());
    let credentials = CredentialManager::new();

    for (id, base_url) in providers {
        registry.register(ProviderDefinition {
            id: id.to_owned(),
            display_name: id.to_owned(),
            base_url,
            auth_type: AuthType::ApiKey,
            model_patterns: vec![],
            compiled_patterns: Vec::new(),
            endpoints: vec!["/chat/completions".to_owned()],
            features: vec!["chat".to_owned()],
            model_count: 1,
            timeout_secs: 30,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });
        credentials.register_keys(id.to_owned(), vec![format!("{id}-key")], 10);
    }

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
    proxy_app::state::AppState::with_parts(rotator, registry)
}

async fn get_models(state: proxy_app::state::AppState) -> Value {
    get_json(state, "/v1/models").await
}

async fn get_json(state: proxy_app::state::AppState, uri: &str) -> Value {
    let response = proxy_app::build_app_with_state(state)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn models_route_parses_openai_gemini_and_partial_failure() {
    let (openai_url, _) = model_server(StatusCode::OK, r#"{"data":[{"id":"gpt-4"}]}"#).await;
    let (gemini_url, _) = model_server(
        StatusCode::OK,
        r#"{"models":[{"name":"models/gemini-pro"}]}"#,
    )
    .await;
    let (broken_url, _) =
        model_server(StatusCode::INTERNAL_SERVER_ERROR, r#"{"error":"boom"}"#).await;

    let body = get_models(test_state(vec![
        ("openai", openai_url),
        ("gemini", gemini_url),
        ("broken", broken_url),
    ]))
    .await;

    let ids: Vec<_> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"openai/gpt-4"));
    assert!(ids.contains(&"gemini/models/gemini-pro"));
    assert!(!ids.contains(&"boom"));
}

#[tokio::test]
async fn models_route_returns_static_models_when_upstream_fails() {
    let (openai_url, _) =
        model_server(StatusCode::INTERNAL_SERVER_ERROR, r#"{"error":"boom"}"#).await;

    let body = get_models(test_state(vec![("openai", openai_url)])).await;
    let ids: Vec<_> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();

    assert!(ids.contains(&"openai/gpt-4o"));
}

#[tokio::test]
async fn models_route_returns_static_models_when_upstream_times_out() {
    // Delay exceeds PROVIDER_MODEL_TIMEOUT (15s) so the slow upstream times out
    // and the route falls back to static models.
    let (slow_openai_url, _) = delayed_model_server(
        StatusCode::OK,
        r#"{"data":[{"id":"slow-live-model"}]}"#,
        Duration::from_secs(16),
    )
    .await;
    let (gemini_url, _) = model_server(
        StatusCode::OK,
        r#"{"models":[{"name":"models/gemini-live"}]}"#,
    )
    .await;

    let started = Instant::now();
    let body = get_models(test_state(vec![
        ("openai", slow_openai_url),
        ("gemini", gemini_url),
    ]))
    .await;
    let ids: Vec<_> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();

    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(ids.contains(&"openai/gpt-4o"));
    assert!(ids.contains(&"gemini/models/gemini-live"));
    assert!(!ids.contains(&"openai/slow-live-model"));
}

#[tokio::test]
async fn models_route_returns_static_models_without_credentials() {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: "openai".to_owned(),
        display_name: "openai".to_owned(),
        base_url: "http://127.0.0.1:1/v1".to_owned(),
        auth_type: AuthType::ApiKey,
        model_patterns: vec![],
        compiled_patterns: vec![],
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });
    let rotator = RotatorClient::new(
        CredentialManager::new(),
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(rotator::RateLimiterRegistry::new()),
        Arc::new(rotator::CooldownManager::new()),
        Arc::new(rotator::CircuitBreakerRegistry::new()),
        None,
        0,
    );

    let body = get_models(proxy_app::state::AppState::with_parts(rotator, registry)).await;
    let ids: Vec<_> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();

    assert!(ids.contains(&"openai/gpt-4o"));
}

#[tokio::test]
async fn models_route_returns_discovered_models_when_upstream_works() {
    let (openai_url, _) = model_server(StatusCode::OK, r#"{"data":[{"id":"live-model"}]}"#).await;

    let body = get_models(test_state(vec![("openai", openai_url)])).await;
    let ids: Vec<_> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();

    assert!(ids.contains(&"openai/live-model"));
    assert!(!ids.contains(&"openai/gpt-4o"));
}

#[tokio::test]
async fn models_route_applies_provider_model_filters() {
    unsafe {
        std::env::set_var("IGNORE_MODELS_FILTERTEST", "gpt-4*");
        std::env::set_var("WHITELIST_MODELS_FILTERTEST", "gpt-4o");
    }

    let (provider_url, _) = model_server(
        StatusCode::OK,
        r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4-turbo"},{"id":"gpt-3.5"}]}"#,
    )
    .await;

    let mut registry = ProviderRegistry::default();
    registry.register(ProviderDefinition {
        id: "filtertest".to_owned(),
        display_name: "filtertest".to_owned(),
        base_url: provider_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![],
        compiled_patterns: vec![],
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });
    registry.load_from_env();
    let registry = Arc::new(registry);
    let credentials = CredentialManager::new();
    credentials.register_keys(
        "filtertest".to_owned(),
        vec!["filtertest-key".to_owned()],
        10,
    );
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
    let body = get_models(proxy_app::state::AppState::with_parts(rotator, registry)).await;
    let ids: Vec<_> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();

    assert!(ids.contains(&"filtertest/gpt-4o"));
    assert!(ids.contains(&"filtertest/gpt-3.5"));
    assert!(!ids.contains(&"filtertest/gpt-4-turbo"));

    unsafe {
        std::env::remove_var("IGNORE_MODELS_FILTERTEST");
        std::env::remove_var("WHITELIST_MODELS_FILTERTEST");
    }
}

#[tokio::test]
async fn models_route_uses_warm_cache() {
    let (openai_url, request_count) =
        model_server(StatusCode::OK, r#"{"data":[{"id":"gpt-4"}]}"#).await;
    let state = test_state(vec![("openai", openai_url)]);

    let first = get_models(state.clone()).await;
    assert_eq!(first["data"].as_array().unwrap().len(), 1);

    let started = Instant::now();
    let second = get_models(state).await;

    assert_eq!(second["data"].as_array().unwrap().len(), 1);
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn models_route_supports_enriched_false() {
    let (openai_url, _) = model_server(StatusCode::OK, r#"{"data":[{"id":"gpt-4"}]}"#).await;

    let body = get_json(
        test_state(vec![("openai", openai_url)]),
        "/v1/models?enriched=false",
    )
    .await;

    assert_eq!(body["object"], "list");
    assert_eq!(body["data"][0]["id"], "openai/gpt-4");
    assert!(body["data"][0].get("pricing").is_none());
}

#[tokio::test]
async fn models_route_supports_enriched_true() {
    let (openai_url, _) = model_server(StatusCode::OK, r#"{"data":[{"id":"gpt-4o"}]}"#).await;

    let body = get_json(
        test_state(vec![("openai", openai_url)]),
        "/v1/models?enriched=true",
    )
    .await;

    assert_eq!(body["object"], "list");
    assert_eq!(body["data"][0]["id"], "openai/gpt-4o");
    assert!(body["data"][0]["pricing"].is_object());
}

#[tokio::test]
async fn providers_route_returns_plain_array() {
    let (openai_url, _) = model_server(StatusCode::OK, r#"{"data":[]}"#).await;

    let body = get_json(test_state(vec![("openai", openai_url)]), "/v1/providers").await;

    assert!(body.as_array().is_some());
    assert_eq!(body[0]["id"], "openai");
}

#[tokio::test]
async fn api_tags_returns_ollama_schema() {
    let (openai_url, _) = model_server(StatusCode::OK, r#"{"data":[{"id":"gpt-4"}]}"#).await;

    let body = get_json(test_state(vec![("openai", openai_url)]), "/api/tags").await;

    assert_eq!(body["models"][0]["name"], "openai/gpt-4");
    assert_eq!(body["models"][0]["model"], "openai/gpt-4");
    assert!(body["models"][0]["details"].is_object());
    assert!(
        body["models"][0]["details"]
            .get("quantization_level")
            .is_some()
    );
}
