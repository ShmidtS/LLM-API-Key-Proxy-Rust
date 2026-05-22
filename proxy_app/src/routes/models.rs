use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, Query, State},
    response::Json,
    routing::get,
};
use futures::future::join_all;
use models::common::ModelInfo;
use rotator::{RotatorClient, parse_model_ids_response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const MODEL_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ModelsQuery {
    enriched: Option<bool>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/api/v1/models", get(list_models))
        .route("/v1/models/{*model_id}", get(get_model))
        .route("/v1/providers", get(list_providers))
        .route("/api/tags", get(ollama_tags))
}

async fn list_models(
    State(state): State<AppState>,
    Query(query): Query<ModelsQuery>,
) -> Json<Value> {
    if query.enriched != Some(false)
        && let Err(error) = state
            .model_info
            .write()
            .await
            .refresh_if_needed(&state.catalog_client)
            .await
    {
        tracing::warn!(error = %error, "failed to refresh model catalog");
    }

    let data = collect_models(&state).await;
    let data: Vec<Value> = data
        .into_iter()
        .map(|model| {
            if query.enriched == Some(false) {
                json!({
                    "id": model.id,
                    "object": model.object,
                    "created": model.created,
                    "owned_by": model.owned_by,
                })
            } else {
                enrich_model(&state, model)
            }
        })
        .collect();

    Json(json!({"object": "list", "data": data}))
}

async fn collect_models(state: &AppState) -> Vec<ModelInfo> {
    let mut tasks = Vec::new();
    let now = Instant::now();

    for provider in state.registry.all_providers() {
        if state
            .rotator
            .credentials
            .get_least_loaded(&provider.id)
            .is_none()
        {
            continue;
        }

        let provider_id = provider.id;
        let cached = state
            .model_cache
            .read()
            .await
            .get(&provider_id)
            .cloned()
            .and_then(|(models, cached_at)| {
                (now.duration_since(cached_at) < MODEL_CACHE_TTL).then_some(models)
            });
        let rotator = state.rotator.clone();
        tasks.push(tokio::spawn(fetch_provider_models(
            provider_id,
            cached,
            rotator,
        )));
    }

    let mut data = Vec::new();
    for result in join_all(tasks).await {
        let Ok((provider_id, models)) = result else {
            continue;
        };
        if let Some(models) = models {
            state
                .model_cache
                .write()
                .await
                .insert(provider_id.clone(), (models.clone(), Instant::now()));
            data.extend(models.into_iter().map(|id| model_info(id, &provider_id)));
        }
    }

    data
}

async fn fetch_provider_models(
    provider_id: String,
    cached: Option<Vec<String>>,
    rotator: Arc<RotatorClient>,
) -> (String, Option<Vec<String>>) {
    if let Some(models) = cached {
        return (provider_id, Some(models));
    }

    let models = match rotator.list_models(&provider_id).await {
        Ok(response) => parse_model_ids_response(&provider_id, response).await,
        Err(error) => {
            tracing::warn!(
                provider = %provider_id,
                error = %error,
                "failed to fetch provider models"
            );
            None
        }
    };
    (provider_id, models)
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

    Json(json!(data))
}

async fn ollama_tags(State(state): State<AppState>) -> Json<Value> {
    let models: Vec<_> = collect_models(&state)
        .await
        .into_iter()
        .map(ollama_model)
        .collect();

    Json(json!({"models": models}))
}

fn model_info(id: String, provider_id: &str) -> ModelInfo {
    ModelInfo {
        id,
        object: "model".into(),
        created: 0,
        owned_by: Some(provider_id.to_owned()),
    }
}

fn enrich_model(state: &AppState, model: ModelInfo) -> Value {
    let metadata = state
        .model_info
        .try_read()
        .ok()
        .and_then(|service| service.get_model(&model.id));

    json!({
        "id": model.id,
        "object": model.object,
        "created": model.created,
        "owned_by": model.owned_by,
        "display_name": metadata.as_ref().map(|metadata| metadata.display_name.clone()),
        "context_length": metadata.as_ref().map(|metadata| metadata.context_length),
        "pricing": metadata.as_ref().map(|metadata| json!({
            "input": metadata.pricing_input_per_1k,
            "output": metadata.pricing_output_per_1k,
        })),
        "capabilities": metadata.map(|metadata| metadata.capabilities),
    })
}

fn ollama_model(model: ModelInfo) -> Value {
    json!({
        "name": model.id,
        "model": model.id,
        "modified_at": "1970-01-01T00:00:00Z",
        "size": 0,
        "digest": "",
        "details": {
            "parent_model": Value::Null,
            "format": Value::Null,
            "family": model.owned_by,
            "families": Value::Null,
            "parameter_size": Value::Null,
            "quantization_level": Value::Null,
        }
    })
}
