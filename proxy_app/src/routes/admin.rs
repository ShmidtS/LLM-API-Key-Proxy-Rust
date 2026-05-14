use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use rotator::CircuitState;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::atomic::Ordering, time::Duration};

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

async fn stats(State(state): State<AppState>) -> Json<Value> {
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
            "base_url": provider.base_url.clone(),
            "status": circuit_status_label(state.rotator.circuit_state(&provider.id)),
            "latency_ms": state.rotator.last_latency_ms(&provider.id),
            "cached_models": cached_models,
            "cache_ttl_secs": cache_ttl_secs,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
        }));
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
    messages: Vec<TokenCountMessage>,
}

#[derive(Deserialize)]
struct TokenCountMessage {
    content: Option<Value>,
}

async fn token_count(
    State(_state): State<AppState>,
    Json(req): Json<TokenCountRequest>,
) -> Json<Value> {
    let _model = req.model;
    let chars = req
        .messages
        .iter()
        .filter_map(|message| message.content.as_ref())
        .map(|content| match content {
            Value::String(text) => text.chars().count(),
            other => other.to_string().chars().count(),
        })
        .sum::<usize>();

    Json(json!({"token_count": chars.div_ceil(4)}))
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
    let prices = HashMap::from([
        ("openai/gpt-4o", (5.0_f64, 15.0_f64)),
        ("anthropic/claude-3-5-sonnet", (3.0_f64, 15.0_f64)),
    ]);
    let (input_price, output_price) = prices
        .get(req.model.as_str())
        .copied()
        .unwrap_or((1.0, 2.0));
    let estimated_cost = (req.input_tokens as f64 * input_price
        + req.output_tokens as f64 * output_price)
        / 1_000_000.0;

    Json(json!({"estimated_cost_usd": estimated_cost}))
}
