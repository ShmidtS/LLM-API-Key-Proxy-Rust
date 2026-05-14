use crate::errors::AppError;
use crate::routes::utils::upstream_response;
use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use models::embeddings::EmbeddingRequest;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/embeddings", post(create_embeddings))
}

async fn create_embeddings(
    State(state): State<AppState>,
    Json(req): Json<EmbeddingRequest>,
) -> Result<Response, AppError> {
    if !state.registry.is_model_allowed(&req.model) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Model not allowed"})),
        )
            .into_response());
    }

    let provider = state
        .registry
        .resolve_provider_by_model(&req.model)
        .unwrap_or("openai");

    let body = serde_json::to_value(&req)?;
    let resp = state.rotator.request(provider, "embeddings", body).await?;
    upstream_response(resp).await
}
