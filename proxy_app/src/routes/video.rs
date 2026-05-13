use axum::{Router, response::Json, routing::post};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/video/generations", post(generations))
}

async fn generations() -> Json<Value> {
    Json(json!({"created": 0, "data": []}))
}
