use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::Value;

use crate::{routes::utils::not_implemented, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/responses", post(create_response))
        .route("/responses/{response_id}", get(get_response))
        .route("/v1/responses", post(create_response))
        .route("/v1/responses/{response_id}", get(get_response))
}

async fn create_response() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn get_response() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}
