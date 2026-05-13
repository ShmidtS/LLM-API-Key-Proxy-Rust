use axum::{Router, response::Json, routing::post};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/moderations", post(moderations))
}

async fn moderations() -> Json<Value> {
    Json(json!({"id": "placeholder", "model": "placeholder", "results": []}))
}
