use axum::{Router, extract::State, response::Json, routing::get};
use serde_json::json;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/version", get(version))
}

pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/v1/props", get(props))
        .route("/props", get(props))
        .route("/v1/model-info/stats", get(model_info_stats))
}

async fn props() -> Json<serde_json::Value> {
    Json(json!({"version": "1.16", "mode": "llm", "gpu_devices": []}))
}

async fn version() -> Json<serde_json::Value> {
    Json(json!({"version": "1.16"}))
}

async fn model_info_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    let model_info = state.model_info.read().await;
    let all_models = model_info.get_all_models();
    let mut provider_counts = std::collections::HashMap::<String, usize>::new();
    for model in &all_models {
        *provider_counts.entry(model.provider.clone()).or_insert(0) += 1;
    }
    Json(json!({
        "object": "model_info.stats",
        "total_models": all_models.len(),
        "provider_counts": provider_counts,
    }))
}
