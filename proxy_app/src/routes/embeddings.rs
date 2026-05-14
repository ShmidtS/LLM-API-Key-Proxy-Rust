use crate::errors::AppError;
use crate::state::AppState;
use axum::{Router, extract::State, response::Json, routing::post};
use models::embeddings::EmbeddingRequest;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/embeddings", post(create_embeddings))
}

async fn create_embeddings(
    State(state): State<AppState>,
    Json(req): Json<EmbeddingRequest>,
) -> Result<Json<Value>, AppError> {
    let provider = state
        .registry
        .find_provider_for_model(&req.model)
        .unwrap_or_else(|| "openai".to_owned());

    let body = serde_json::to_value(&req)?;
    let resp = state.rotator.request(&provider, "embeddings", body).await?;
    let data = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data))
}
