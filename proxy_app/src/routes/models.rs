use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, State},
    response::Json,
    routing::get,
};
use futures::future::join_all;
use models::common::{ModelInfo, ModelList};
use rotator::{RotatorClient, parse_model_ids_response};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const MODEL_CACHE_TTL: Duration = Duration::from_secs(300);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/api/v1/models", get(list_models))
        .route("/v1/models/{*model_id}", get(get_model))
        .route("/v1/providers", get(list_providers))
        .route("/api/tags", get(ollama_tags))
}

async fn list_models(State(state): State<AppState>) -> Json<ModelList> {
    if let Err(error) = state
        .model_info
        .write()
        .await
        .refresh_if_needed(&state.catalog_client)
        .await
    {
        tracing::warn!(error = %error, "failed to refresh model catalog");
    }

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

    Json(ModelList {
        object: "list".into(),
        data,
    })
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

fn model_info(id: String, provider_id: &str) -> ModelInfo {
    ModelInfo {
        id,
        object: "model".into(),
        created: 0,
        owned_by: Some(provider_id.to_owned()),
    }
}
