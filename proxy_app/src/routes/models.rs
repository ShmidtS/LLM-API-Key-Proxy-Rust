use crate::state::AppState;
use axum::{Router, extract::State, response::Json, routing::get};
use models::common::{ModelInfo, ModelList};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/models/{model_id}", get(get_model))
        .route("/v1/providers", get(list_providers))
}

async fn list_models(State(_state): State<AppState>) -> Json<ModelList> {
    Json(ModelList {
        object: "list".into(),
        data: vec![],
    })
}

async fn get_model(
    State(_state): State<AppState>,
    axum::extract::Path(_model_id): axum::extract::Path<String>,
) -> Json<ModelInfo> {
    Json(ModelInfo {
        id: "placeholder".into(),
        object: "model".into(),
        created: 0,
        owned_by: Some("proxy".into()),
    })
}

async fn list_providers(State(_state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"object": "list", "data": []}))
}
