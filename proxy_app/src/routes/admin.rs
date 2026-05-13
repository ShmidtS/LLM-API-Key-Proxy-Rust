use axum::{Router, extract::State, response::Json, routing::get};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/stats", get(quota_stats))
        .route("/v1/admin/usage", get(usage_overview))
}

async fn quota_stats(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "admin.stats"}))
}

async fn usage_overview(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "admin.usage"}))
}
