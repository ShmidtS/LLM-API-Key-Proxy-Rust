use axum::{
    Json, Router,
    extract::{Query, State},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rotator::CircuitState;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::atomic::Ordering, time::Duration};

use crate::errors::invalid_request_error;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/stats", get(stats))
        .route("/admin/token_count", post(token_count))
        .route("/admin/cost_estimate", post(cost_estimate))
        .route("/admin/errors", get(errors))
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

#[derive(Deserialize)]
struct QuotaStatsQuery {
    provider: Option<String>,
    format: Option<String>,
}

async fn errors(State(state): State<AppState>) -> Response {
    let json_str = if let Some(journal) = state.rotator.error_journal() {
        journal.export_json()
    } else {
        "{}".to_owned()
    };
    let parsed = serde_json::from_str::<serde_json::Value>(&json_str)
        .unwrap_or_else(|_| json!({"error": "invalid json"}));
    Json(parsed).into_response()
}

async fn quota_stats_get(
    State(state): State<AppState>,
    Query(query): Query<QuotaStatsQuery>,
) -> Response {
    quota_stats_view_response(
        &state,
        query.provider.as_deref(),
        query.format.as_deref(),
        None,
    )
    .await
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
    quota_stats_view_response(
        &state,
        req.provider.as_deref(),
        None,
        refresh_requested.then_some(json!({"supported": false})),
    )
    .await
}

struct AdminStats {
    providers_array: Vec<Value>,
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
    let mut providers = Map::new();
    let mut total_credentials = 0usize;
    let mut active_credentials = 0usize;
    let mut exhausted_credentials = 0usize;
    let mut total_requests = 0u64;
    let mut input_uncached = 0u64;
    let input_cached = 0u64;
    let mut output = 0u64;
    let mut total_cost = 0.0;
    let cooldowns = cooldown_map(state);

    for provider in state.registry.all_providers() {
        if provider_filter.is_some_and(|filter| filter != provider.id) {
            continue;
        }

        let keys = state
            .rotator
            .credentials
            .credentials
            .get(&provider.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let usage_by_key: HashMap<_, _> = stats
            .usage
            .iter()
            .filter(|entry| entry.provider == provider.id)
            .map(|entry| (entry.key.clone(), entry.clone()))
            .collect();
        let provider_prompt = usage_by_key
            .values()
            .map(|entry| u64::from(entry.prompt_tokens))
            .sum::<u64>();
        let provider_completion = usage_by_key
            .values()
            .map(|entry| u64::from(entry.completion_tokens))
            .sum::<u64>();
        let provider_requests = usage_by_key.len() as u64;
        let provider_cost = estimate_usage_cost(provider_prompt, provider_completion);
        let active_count = keys
            .iter()
            .filter(|credential| {
                credential.current_requests.load(Ordering::Relaxed) < credential.concurrent_limit
            })
            .count();
        let provider_cooldowns = cooldowns.get(&provider.id);
        let credentials = keys
            .iter()
            .enumerate()
            .map(|(idx, credential)| {
                let usage = usage_by_key.get(&credential.key);
                let key_cooldown_remaining = provider_cooldowns
                    .and_then(|items| items.get(&credential.key))
                    .copied();
                let status = if key_cooldown_remaining.is_some() {
                    "cooldown"
                } else if credential.current_requests.load(Ordering::Relaxed)
                    >= credential.concurrent_limit
                {
                    "exhausted"
                } else {
                    "active"
                };
                json!({
                    "identifier": format!("{}-{}", provider.id, idx + 1),
                    "email": Value::Null,
                    "tier": "",
                    "status": status,
                    "requests": usage.map_or(0, |_| 1),
                    "tokens": {
                        "input_cached": 0,
                        "input_uncached": usage.map_or(0, |entry| entry.prompt_tokens),
                        "input_cache_pct": 0,
                        "output": usage.map_or(0, |entry| entry.completion_tokens),
                    },
                    "approx_cost": usage.map(|entry| estimate_usage_cost(
                        u64::from(entry.prompt_tokens),
                        u64::from(entry.completion_tokens),
                    )),
                    "last_used_ts": usage.map(|entry| entry.timestamp),
                    "key_cooldown_remaining": key_cooldown_remaining,
                    "model_cooldowns": {},
                    "models": {},
                })
            })
            .collect::<Vec<_>>();

        total_credentials += keys.len();
        active_credentials += active_count;
        exhausted_credentials += keys.len().saturating_sub(active_count);
        total_requests += provider_requests;
        input_uncached += provider_prompt;
        output += provider_completion;
        total_cost += provider_cost;

        providers.insert(
            provider.id.clone(),
            json!({
                "id": provider.id,
                "display_name": provider.display_name,
                "base_url": provider.base_url,
                "endpoints": provider.endpoints,
                "features": provider.features,
                "model_count": provider.model_count,
                "status": circuit_status_label(state.rotator.circuit_state(&provider.id)),
                "latency_ms": state.rotator.last_latency_ms(&provider.id),
                "credential_count": keys.len(),
                "active_count": active_count,
                "exhausted_count": keys.len().saturating_sub(active_count),
                "total_requests": provider_requests,
                "tokens": token_summary(input_cached, provider_prompt, provider_completion),
                "approx_cost": optional_cost(provider_cost),
                "credentials": credentials,
                "quota_groups": {},
            }),
        );
    }

    json!({
        "providers": providers,
        "summary": {
            "total_providers": providers.len(),
            "total_credentials": total_credentials,
            "active_credentials": active_credentials,
            "exhausted_credentials": exhausted_credentials,
            "total_requests": total_requests,
            "tokens": token_summary(input_cached, input_uncached, output),
            "approx_total_cost": optional_cost(total_cost),
            "total_keys": stats.total_keys,
            "active_requests": stats.active_requests,
            "provider_count": providers.len(),
            "usage_entries": stats.usage.len(),
        },
        "global_summary": {
            "total_providers": providers.len(),
            "total_credentials": total_credentials,
            "total_requests": total_requests,
            "tokens": token_summary(input_cached, input_uncached, output),
            "approx_total_cost": optional_cost(total_cost),
        },
        "data_source": "cache",
        "timestamp": chrono::Utc::now().timestamp(),
        "timestamp_iso": chrono::Utc::now().to_rfc3339(),
        "refresh_result": refresh_result,
    })
}

async fn quota_stats_view_response(
    state: &AppState,
    provider_filter: Option<&str>,
    format: Option<&str>,
    refresh_result: Option<Value>,
) -> Response {
    let response = quota_stats_response(state, provider_filter, refresh_result).await;
    match format.unwrap_or("json") {
        "text" => (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            format_quota_text(&response),
        )
            .into_response(),
        "html" => Html(format_quota_html(&response)).into_response(),
        _ => Json(response).into_response(),
    }
}

fn token_summary(input_cached: u64, input_uncached: u64, output: u64) -> Value {
    let total_input = input_cached + input_uncached;
    let input_cache_pct = if total_input > 0 {
        (input_cached as f64 / total_input as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    json!({
        "input_cached": input_cached,
        "input_uncached": input_uncached,
        "input_cache_pct": input_cache_pct,
        "output": output,
    })
}

fn optional_cost(cost: f64) -> Value {
    if cost > 0.0 { json!(cost) } else { Value::Null }
}

fn estimate_usage_cost(input_tokens: u64, output_tokens: u64) -> f64 {
    rotator::costs::estimate_cost_breakdown("gpt-4o", input_tokens, output_tokens, None, None)
        .map(|breakdown| breakdown.total_cost)
        .unwrap_or(0.0)
}

fn cooldown_map(state: &AppState) -> HashMap<String, HashMap<String, u64>> {
    let mut result: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for (provider, key, remaining) in state.rotator.active_cooldowns() {
        result
            .entry(provider)
            .or_default()
            .insert(key, remaining.as_secs());
    }
    result
}

fn format_quota_text(response: &Value) -> String {
    let mut lines = vec!["Quota & Usage Statistics".to_owned()];
    if let Some(providers) = response.get("providers").and_then(Value::as_object) {
        for (provider, stats) in providers {
            let credentials = stats
                .get("credential_count")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let requests = stats
                .get("total_requests")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let tokens = stats.get("tokens").unwrap_or(&Value::Null);
            let input = tokens
                .get("input_cached")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                + tokens
                    .get("input_uncached")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
            let output = tokens
                .get("output")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let cost = format_cost(stats.get("approx_cost").and_then(Value::as_f64));
            lines.push(format!(
                "{provider}: {credentials} credentials | {requests} requests | {}/{} tokens | {cost} cost",
                format_tokens(input),
                format_tokens(output),
            ));
        }
    }
    lines.join("\n")
}

fn format_quota_html(response: &Value) -> String {
    let mut rows = String::new();
    if let Some(providers) = response.get("providers").and_then(Value::as_object) {
        for (provider, stats) in providers {
            let credentials = stats
                .get("credential_count")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let requests = stats
                .get("total_requests")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let cost = format_cost(stats.get("approx_cost").and_then(Value::as_f64));
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(provider),
                credentials,
                requests,
                cost
            ));
        }
    }
    format!(
        "<!doctype html><html><head><title>Quota & Usage Statistics</title></head><body><h1>Quota & Usage Statistics</h1><table><thead><tr><th>Provider</th><th>Credentials</th><th>Requests</th><th>Cost</th></tr></thead><tbody>{rows}</tbody></table></body></html>"
    )
}

fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{}k", count / 1_000)
    } else {
        count.to_string()
    }
}

fn format_cost(cost: Option<f64>) -> String {
    let Some(cost) = cost else {
        return "-".to_owned();
    };
    if cost == 0.0 {
        "-".to_owned()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
