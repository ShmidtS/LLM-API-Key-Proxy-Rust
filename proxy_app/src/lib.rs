pub mod errors;
pub mod middleware;
pub mod routes;
pub mod state;

use axum::{
    Router,
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::{from_fn, from_fn_with_state},
};
use proxy_config::ProxyConfig;
use std::time::Duration;
use tower_http::{
    compression::CompressionLayer,
    cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
};

pub fn build_app() -> Router {
    build_app_with_state(state::AppState::new())
}

fn cors_layer(config: &ProxyConfig) -> Option<CorsLayer> {
    if config.cors_allowed_origins.is_empty() {
        return None;
    }

    let origins = config
        .cors_allowed_origins
        .iter()
        .filter_map(|origin| match HeaderValue::from_str(origin) {
            Ok(origin) => Some(origin),
            Err(_) => {
                tracing::warn!("ignoring invalid cors_allowed_origins entry: {}", origin);
                None
            }
        });
    let mut cors = CorsLayer::new().allow_origin(AllowOrigin::list(origins));

    if !config.cors_allowed_methods.is_empty() {
        let methods = config
            .cors_allowed_methods
            .iter()
            .filter_map(|method| match method.parse::<Method>() {
                Ok(method) => Some(method),
                Err(_) => {
                    tracing::warn!("ignoring invalid cors_allowed_methods entry: {}", method);
                    None
                }
            });
        cors = cors.allow_methods(AllowMethods::list(methods));
    }

    if !config.cors_allowed_headers.is_empty() {
        let headers = config
            .cors_allowed_headers
            .iter()
            .filter_map(|header| match header.parse::<HeaderName>() {
                Ok(header) => Some(header),
                Err(_) => {
                    tracing::warn!("ignoring invalid cors_allowed_headers entry: {}", header);
                    None
                }
            });
        cors = cors.allow_headers(AllowHeaders::list(headers));
    }

    Some(cors)
}

pub fn build_app_with_state(app_state: state::AppState) -> Router {
    let config = app_state.config.clone();
    let cors = cors_layer(&config);

    let protected = Router::new()
        .merge(routes::admin::router())
        .merge(routes::tools::router())
        .merge(routes::agents::router())
        .route_layer(from_fn_with_state(
            app_state.clone(),
            middleware::require_admin_auth,
        ));

    let proxy_protected = Router::new()
        .merge(routes::chat::router())
        .merge(routes::embeddings::router())
        .merge(routes::anthropic::router())
        .merge(routes::files::router())
        .merge(routes::batches::router())
        .merge(routes::audio::router())
        .merge(routes::images::router())
        .merge(routes::video::router())
        .merge(routes::responses::router())
        .merge(routes::moderation::router())
        .route_layer(from_fn_with_state(
            app_state.clone(),
            middleware::require_proxy_auth,
        ));

    let app = Router::new()
        .route(
            "/",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"Status": "API Key Proxy is running"}))
            }),
        )
        .merge(routes::health::router())
        .merge(routes::models::router())
        .merge(proxy_protected)
        .merge(protected)
        .layer(from_fn(middleware::security_headers));

    let app = if let Some(cors) = cors {
        app.layer(cors)
    } else {
        app
    };

    app.layer(CompressionLayer::new())
        .layer(from_fn(middleware::add_request_id))
        .layer(RequestBodyLimitLayer::new(config.max_body_bytes))
        .layer(from_fn_with_state(
            app_state.clone(),
            middleware::reject_oversized_body,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(config.request_timeout_secs),
        ))
        .layer(from_fn_with_state(
            app_state.clone(),
            middleware::log_requests,
        ))
        .with_state(app_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use proxy_config::ProxyConfig;
    use tower::ServiceExt;

    #[tokio::test]
    async fn omits_cors_headers_when_origins_are_empty() {
        let config = ProxyConfig::default();
        let app = build_app_with_state(state::AppState::from_config(config));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::ORIGIN, "https://example.com")
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
    async fn allows_configured_cors_origin_methods_and_headers() {
        let mut config = ProxyConfig::default();
        config.cors_allowed_origins = vec!["https://example.com".to_owned()];
        config.cors_allowed_methods = vec!["GET".to_owned()];
        config.cors_allowed_headers = vec!["authorization".to_owned()];
        let app = build_app_with_state(state::AppState::from_config(config));

        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/")
                    .header(header::ORIGIN, "https://example.com")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            "GET"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
                .unwrap(),
            "authorization"
        );
    }

    #[tokio::test]
    async fn skips_invalid_cors_entries() {
        let mut config = ProxyConfig::default();
        config.cors_allowed_origins =
            vec!["not a header".to_owned(), "https://example.com".to_owned()];
        config.cors_allowed_methods = vec!["NOT A METHOD".to_owned(), "GET".to_owned()];
        config.cors_allowed_headers = vec!["not a header".to_owned(), "authorization".to_owned()];
        let app = build_app_with_state(state::AppState::from_config(config));

        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/")
                    .header(header::ORIGIN, "https://example.com")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            "GET"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
                .unwrap(),
            "authorization"
        );
    }

    #[tokio::test]
    async fn adds_request_id_header() {
        let app = build_app_with_state(state::AppState::new());

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert!(response.headers().get("x-request-id").is_some());
    }
}
