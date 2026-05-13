use crate::errors::AppError;
use crate::state::AppState;
use axum::{Router, extract::State, response::Json, routing::post};
use models::embeddings::{EmbeddingRequest, EmbeddingResponse};
use models::usage::TokenUsage;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/embeddings", post(create_embeddings))
}

async fn create_embeddings(
    State(_state): State<AppState>,
    Json(req): Json<EmbeddingRequest>,
) -> Result<Json<EmbeddingResponse>, AppError> {
    Ok(Json(EmbeddingResponse {
        object: "list".into(),
        data: vec![],
        model: req.model,
        usage: TokenUsage::default(),
    }))
}
