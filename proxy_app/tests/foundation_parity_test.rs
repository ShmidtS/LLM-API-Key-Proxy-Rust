use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use proxy_app::state::AppState;
use proxy_config::ProxyConfig;
use serde_json::{Value, json};
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
