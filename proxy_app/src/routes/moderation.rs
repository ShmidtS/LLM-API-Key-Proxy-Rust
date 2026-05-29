use crate::errors::AppError;
use crate::routes::utils::{normalize_model_in_body, upstream_response};
use crate::state::AppState;
use axum::response::Response;
use axum::{Router, extract::State, response::Json, routing::post};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/moderations", post(moderations))
}

async fn moderations(
    State(state): State<AppState>,
    Json(mut req): Json<Value>,
) -> Result<Response, AppError> {
    normalize_model_in_body(&mut req, "openai");
    let upstream = state.rotator.request("openai", "moderations", req).await?;
    upstream_response(upstream).await
}
