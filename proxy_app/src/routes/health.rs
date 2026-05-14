use axum::{Router, response::Json, routing::get};
use serde_json::json;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/props", get(props))
        .route("/props", get(props))
        .route("/version", get(version))
        .route("/v1/model-info/stats", get(model_info_stats))
}

async fn props() -> Json<serde_json::Value> {
    Json(json!({"object": "props"}))
}

async fn version() -> Json<serde_json::Value> {
    Json(json!({"version": env!("CARGO_PKG_VERSION")}))
}

async fn model_info_stats() -> Json<serde_json::Value> {
    Json(json!({"object": "model_info.stats"}))
}
