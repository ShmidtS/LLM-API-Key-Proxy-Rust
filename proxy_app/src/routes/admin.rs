use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use rotator::CircuitState;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/stats", get(stats))
        .route("/admin/token_count", post(token_count))
        .route("/admin/cost_estimate", post(cost_estimate))
        .route("/v1/quota-stats", get(stats).post(stats))
        .route("/v1/token-count", post(token_count))
        .route("/v1/cost-estimate", post(cost_estimate))
}

async fn stats(State(state): State<AppState>, body: Option<Json<Value>>) -> Json<Value> {
    let credentials = &state.rotator.credentials;
    let model_cache = state.model_cache.read().await;
    let usage = state.rotator.usage_entries();
    let mut providers = Vec::new();
    let mut total_keys = 0usize;
    let mut active_requests = 0usize;

    for provider in state.registry.all_providers() {
        let keys = credentials
            .credentials
            .get(&provider.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        total_keys += keys.len();
        active_requests += keys
            .iter()
            .map(|credential| credential.current_requests.load(Ordering::Relaxed))
            .sum::<usize>();
        let (cached_models, cache_ttl_secs) = model_cache
            .get(&provider.id)
            .map(|(models, cached_at)| {
                (
                    models.len(),
                    Duration::from_secs(300)
                        .saturating_sub(cached_at.elapsed())
                        .as_secs(),
                )
            })
            .unwrap_or((0, 300));
        let (prompt_tokens, completion_tokens) = usage
            .iter()
            .filter(|entry| entry.provider == provider.id)
            .fold((0u64, 0u64), |(prompt, completion), entry| {
                (
                    prompt + u64::from(entry.prompt_tokens),
                    completion + u64::from(entry.completion_tokens),
                )
            });

        providers.push(json!({
            "id": provider.id.clone(),
            "display_name": provider.display_name.clone(),
            "base_url": provider.base_url.clone(),
            "endpoints": provider.endpoints.clone(),
            "features": provider.features.clone(),
            "model_count": provider.model_count,
            "status": circuit_status_label(state.rotator.circuit_state(&provider.id)),
            "latency_ms": state.rotator.last_latency_ms(&provider.id),
            "cached_models": cached_models,
            "cache_ttl_secs": cache_ttl_secs,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
        }));
    }

    if let Some(Json(filter)) = body
        && let Some(provider_id) = filter.get("provider").and_then(Value::as_str)
    {
        providers.retain(|p| p.get("id").and_then(Value::as_str) == Some(provider_id));
    }

    providers.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });

    Json(json!({
        "providers": providers,
        "usage": usage,
        "total_keys": total_keys,
        "active_requests": active_requests,
    }))
}

fn circuit_status_label(state: CircuitState) -> &'static str {
    match state {
        CircuitState::Closed | CircuitState::HalfOpen => "OK",
        CircuitState::Open => "Circuit Open",
    }
}

#[derive(Deserialize)]
struct TokenCountRequest {
    model: String,
    messages: Vec<Value>,
}

async fn token_count(
    State(_state): State<AppState>,
    Json(req): Json<TokenCountRequest>,
) -> Json<Value> {
    let token_count = rotator::tokenizer::count_chat_tokens(&req.messages, &req.model);

    Json(json!({"token_count": token_count}))
}

#[derive(Deserialize)]
struct CostEstimateRequest {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
}

async fn cost_estimate(
    State(_state): State<AppState>,
    Json(req): Json<CostEstimateRequest>,
) -> Json<Value> {
    let estimated_cost = rotator::costs::estimate_cost(
        &req.model,
        req.input_tokens as usize,
        req.output_tokens as usize,
    );

    Json(json!({"estimated_cost_usd": estimated_cost}))
}
