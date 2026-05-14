use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, State},
    response::Json,
    routing::get,
};
use models::common::{ModelInfo, ModelList};
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/api/v1/models", get(list_models))
        .route("/v1/models/{model_id}", get(get_model))
        .route("/v1/providers", get(list_providers))
        .route("/api/tags", get(ollama_tags))
}

async fn list_models(State(state): State<AppState>) -> Json<ModelList> {
    let mut data = Vec::new();

    for provider in state.registry.all_providers() {
        if state
            .rotator
            .credentials
            .get_least_loaded(&provider.id)
            .is_none()
        {
            continue;
        }

        match state
            .rotator
            .request(&provider.id, "models", serde_json::json!({}))
            .await
        {
            Ok(response) => match response.json::<Value>().await {
                Ok(value) => data.extend(models_from_value(value, &provider.id)),
                Err(error) => tracing::warn!(
                    provider = %provider.id,
                    error = %error,
                    "failed to decode provider models"
                ),
            },
            Err(error) => tracing::warn!(
                provider = %provider.id,
                error = %error,
                "failed to fetch provider models"
            ),
        }
    }

    Json(ModelList {
        object: "list".into(),
        data,
    })
}

async fn get_model(State(state): State<AppState>, Path(model_id): Path<String>) -> Json<ModelInfo> {
    let owned_by = state
        .registry
        .find_provider_for_model(&model_id)
        .unwrap_or_else(|| "unknown".into());

    Json(ModelInfo {
        id: model_id,
        object: "model".into(),
        created: 0,
        owned_by: Some(owned_by),
    })
}

async fn list_providers(State(state): State<AppState>) -> Json<Value> {
    let data: Vec<_> = state
        .registry
        .all_providers()
        .into_iter()
        .map(|provider| json!({"id": provider.id, "base_url": provider.base_url}))
        .collect();

    Json(json!({"object": "list", "data": data}))
}

async fn ollama_tags(State(state): State<AppState>) -> Json<Value> {
    let models: Vec<_> = state
        .registry
        .all_providers()
        .into_iter()
        .flat_map(|provider| provider.model_patterns)
        .map(|name| json!({"name": name}))
        .collect();

    Json(json!({"models": models}))
}

fn models_from_value(value: Value, provider_id: &str) -> Vec<ModelInfo> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.to_owned();
            let object = model
                .get("object")
                .and_then(Value::as_str)
                .unwrap_or("model")
                .to_owned();
            let created = model.get("created").and_then(Value::as_i64).unwrap_or(0);
            let owned_by = model
                .get("owned_by")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| Some(provider_id.to_owned()));

            Some(ModelInfo {
                id,
                object,
                created,
                owned_by,
            })
        })
        .collect()
}
