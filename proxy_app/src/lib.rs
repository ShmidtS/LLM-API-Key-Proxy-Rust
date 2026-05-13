pub mod errors;
pub mod middleware;
pub mod routes;
pub mod state;

use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

pub fn build_app() -> axum::Router {
    let app_state = state::AppState::new();

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    axum::Router::new()
        .route(
            "/",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"Status": "API Key Proxy is running"}))
            }),
        )
        .merge(routes::chat::router())
        .merge(routes::embeddings::router())
        .merge(routes::anthropic::router())
        .merge(routes::models::router())
        .merge(routes::files::router())
        .merge(routes::batches::router())
        .merge(routes::admin::router())
        .merge(routes::tools::router())
        .merge(routes::agents::router())
        .merge(routes::audio::router())
        .merge(routes::images::router())
        .merge(routes::video::router())
        .merge(routes::moderation::router())
        .layer(cors)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(app_state)
}
