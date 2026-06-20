use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use proxy_config::ProxyConfig;
use serde_json::{Value, json};
use tower::ServiceExt;

fn state_with_auth() -> AppState {
    let config = ProxyConfig {
        admin_token: Some("admin-secret".to_owned()),
        api_keys: vec!["proxy-secret".to_owned()],
        ..Default::default()
    };
    AppState::from_config(config)
}

fn state_without_auth() -> AppState {
    AppState::from_config(ProxyConfig::default())
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

#[tokio::test]
async fn proxy_route_without_api_key_returns_openai_auth_error() {
    let response = proxy_app::build_app_with_state(state_with_auth())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
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
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "401");
}

#[tokio::test]
async fn proxy_route_accepts_x_api_key() {
    let response = proxy_app::build_app_with_state(state_with_auth())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "proxy-secret")
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

    // The proxy must accept the `x-api-key` and pass the request past the auth
    // middleware. A subsequent 401 here comes from the *upstream* provider (e.g.
    // an invalid/expired OPENAI_API_KEY in the test env), not from our auth gate.
    // Distinguish by error shape: proxy auth errors carry
    // `type=authentication_error`; upstream errors do not.
    let (status, body) = response_json(response).await;
    assert_ne!(
        body["error"]["type"],
        "authentication_error",
        "proxy auth middleware rejected a valid x-api-key (status={status})"
    );
}

#[tokio::test]
async fn admin_route_without_admin_token_returns_openai_auth_error() {
    let response = proxy_app::build_app_with_state(state_with_auth())
        .oneshot(
            Request::builder()
                .uri("/admin/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let (status, body) = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "401");
}

#[tokio::test]
async fn models_route_requires_proxy_auth_when_key_configured() {
    let response = proxy_app::build_app_with_state(state_with_auth())
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn version_route_remains_public() {
    let response = proxy_app::build_app_with_state(state_with_auth())
        .oneshot(
            Request::builder()
                .uri("/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn props_tags_and_providers_require_proxy_auth_when_key_configured() {
    for uri in ["/v1/props", "/api/tags", "/v1/providers"] {
        let response = proxy_app::build_app_with_state(state_with_auth())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn proxy_routes_are_open_when_no_proxy_auth_configured() {
    for uri in ["/v1/models", "/v1/props", "/api/tags", "/v1/providers"] {
        let response = proxy_app::build_app_with_state(state_without_auth())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}
