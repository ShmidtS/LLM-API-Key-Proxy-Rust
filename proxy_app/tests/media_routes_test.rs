use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;

fn authed_app() -> axum::Router {
    let mut state = AppState::new();
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    proxy_app::build_app_with_state(state)
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
async fn test_media_get_routes_return_not_implemented() {
    let routes = ["/v1/images/img_123", "/v1/video/vid_123/status"];

    for route in routes {
        let app = authed_app();
        let response = app
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

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED, "{route}");

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Not Implemented", "{route}");
    }
}
