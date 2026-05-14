use axum::{
    Router,
    extract::State,
    response::Json,
    routing::{get, post},
};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/quota-stats", get(quota_stats).post(quota_stats))
        .route("/v1/token-count", post(token_count))
        .route("/v1/cost-estimate", post(cost_estimate))
}

async fn quota_stats(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "admin.stats"}))
}

async fn token_count(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "admin.token_count"}))
}

async fn cost_estimate(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "admin.cost_estimate"}))
}
