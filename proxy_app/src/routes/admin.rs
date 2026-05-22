use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rotator::CircuitState;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{sync::atomic::Ordering, time::Duration};

use crate::errors::invalid_request_error;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/stats", get(stats))
        .route("/admin/token_count", post(token_count))
        .route("/admin/cost_estimate", post(cost_estimate))
        .route(
            "/v1/quota-stats",
            get(quota_stats_get).post(quota_stats_post),
        )
        .route("/v1/token-count", post(token_count))
        .route("/v1/cost-estimate", post(cost_estimate))
}

async fn stats(State(state): State<AppState>, body: Option<Json<Value>>) -> Json<Value> {
    let stats = collect_stats(
        &state,
        body.as_ref()
            .and_then(|Json(filter)| filter.get("provider").and_then(Value::as_str)),
    )
    .await;

    Json(json!({
        "providers": stats.providers_array,
        "usage": stats.usage,
        "total_keys": stats.total_keys,
        "active_requests": stats.active_requests,
    }))
}

async fn quota_stats_get(State(state): State<AppState>) -> Json<Value> {
    Json(quota_stats_response(&state, None, None).await)
}

#[derive(Deserialize)]
struct QuotaStatsRequest {
    action: Option<String>,
    scope: Option<String>,
    provider: Option<String>,
    credential: Option<String>,
    reload: Option<bool>,
    force_refresh: Option<bool>,
}

async fn quota_stats_post(
    State(state): State<AppState>,
    Json(req): Json<QuotaStatsRequest>,
) -> Response {
    if let Some(action) = req.action.as_deref()
        && !matches!(action, "reload" | "force_refresh" | "refresh")
    {
        return invalid_request_error(format!("Unsupported quota-stats action: {action}"))
            .into_response();
    }

    if let Some(scope) = req.scope.as_deref()
        && !matches!(scope, "all" | "provider" | "credential")
    {
        return invalid_request_error(format!("Unsupported quota-stats scope: {scope}"))
            .into_response();
    }

    let refresh_requested = req.action.is_some()
        || req.reload == Some(true)
        || req.force_refresh == Some(true)
        || req.credential.is_some();
    Json(
        quota_stats_response(
            &state,
            req.provider.as_deref(),
            refresh_requested.then_some(json!({"supported": false})),
        )
        .await,
    )
    .into_response()
}

struct AdminStats {
    providers_array: Vec<Value>,
    providers_map: Map<String, Value>,
    usage: Vec<rotator::UsageEntry>,
    total_keys: usize,
    active_requests: usize,
}

async fn collect_stats(state: &AppState, provider_filter: Option<&str>) -> AdminStats {
    let credentials = &state.rotator.credentials;
    let model_cache = state.model_cache.read().await;
    let usage = state.rotator.usage_entries();
    let mut providers_array = Vec::new();
    let mut providers_map = Map::new();
    let mut total_keys = 0usize;
    let mut active_requests = 0usize;

    for provider in state.registry.all_providers() {
        if provider_filter.is_some_and(|filter| filter != provider.id) {
            continue;
        }

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
                    Duration::from_secs(60)
                        .saturating_sub(cached_at.elapsed())
                        .as_secs(),
                )
            })
            .unwrap_or((0, 60));
        let (prompt_tokens, completion_tokens) = usage
            .iter()
            .filter(|entry| entry.provider == provider.id)
            .fold((0u64, 0u64), |(prompt, completion), entry| {
                (
                    prompt + u64::from(entry.prompt_tokens),
                    completion + u64::from(entry.completion_tokens),
                )
            });

        let provider_json = json!({
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
        });
        providers_map.insert(provider.id.clone(), provider_json.clone());
        providers_array.push(provider_json);
    }

    providers_array.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });

    AdminStats {
        providers_array,
        providers_map,
        usage,
        total_keys,
        active_requests,
    }
}

async fn quota_stats_response(
    state: &AppState,
    provider_filter: Option<&str>,
    refresh_result: Option<Value>,
) -> Value {
    let stats = collect_stats(state, provider_filter).await;
    json!({
        "providers": stats.providers_map,
        "summary": {
            "total_keys": stats.total_keys,
            "active_requests": stats.active_requests,
            "provider_count": stats.providers_array.len(),
            "usage_entries": stats.usage.len(),
        },
        "data_source": "cache",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "refresh_result": refresh_result,
    })
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
    #[serde(alias = "input_tokens")]
    prompt_tokens: u64,
    #[serde(alias = "output_tokens")]
    completion_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
}

async fn cost_estimate(
    State(_state): State<AppState>,
    Json(req): Json<CostEstimateRequest>,
) -> Json<Value> {
    let breakdown = rotator::costs::estimate_cost_breakdown(
        &req.model,
        req.prompt_tokens,
        req.completion_tokens,
        req.cache_read_tokens,
        req.cache_creation_tokens,
    );
    let cost = breakdown
        .map(|breakdown| breakdown.total_cost)
        .unwrap_or(0.0);

    Json(json!({
        "model": req.model,
        "cost": cost,
        "currency": "USD",
        "pricing": breakdown,
        "source": if breakdown.is_some() { "static" } else { "fallback" },
    }))
}
