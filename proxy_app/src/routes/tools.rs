use axum::{Router, extract::State, response::Json, routing::post};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/tools/web-search", post(web_search))
        .route("/v1/tools/tokenizer", post(tokenizer))
        .route("/v1/tools/layout-parsing", post(layout))
        .route("/v1/tools/web-reader", post(web_reader))
}

async fn web_search(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "tools.web_search"}))
}

async fn tokenizer(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "tools.tokenizer"}))
}

async fn layout(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "tools.layout"}))
}

async fn web_reader(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "tools.web_reader"}))
}
