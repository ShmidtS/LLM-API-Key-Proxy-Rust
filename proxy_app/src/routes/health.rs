use axum::{Router, response::Json};
use serde_json::json;

pub fn router() -> Router {
    Router::new().route(
        "/",
        axum::routing::get(|| async { Json(json!({"Status": "API Key Proxy is running"})) }),
    )
}
