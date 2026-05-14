use axum::{Router, response::Json, routing::post};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/responses", post(create_response))
}

async fn create_response() -> Json<Value> {
    Json(json!({"id": "resp_placeholder", "object": "response"}))
}
