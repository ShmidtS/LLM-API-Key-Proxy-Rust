use crate::errors::AppError;
use crate::routes::utils::upstream_response;
use crate::state::AppState;
use axum::response::Response;
use axum::{Router, extract::State, response::Json, routing::post};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/moderations", post(moderations))
}

async fn moderations(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state.rotator.request("openai", "moderations", req).await?;
    upstream_response(upstream).await
}
