use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::Value;

use crate::{routes::utils::not_implemented, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tools/web_search", post(web_search))
        .route("/tools/tokenizer", post(tokenizer))
        .route("/tools/layout", post(layout))
        .route("/tools/web_reader", post(web_reader))
        .route("/v1/tools/web-search", post(web_search))
        .route("/v1/tools/tokenizer", post(tokenizer))
        .route("/v1/tools/layout-parsing", post(layout))
        .route("/v1/tools/web-reader", post(web_reader))
}

async fn web_search() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn tokenizer() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn layout() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn web_reader() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}
