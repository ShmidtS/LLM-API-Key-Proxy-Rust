use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use std::{path::PathBuf, time::Instant};
use uuid::Uuid;

use crate::state::AppState;

const REDACTED: &str = "***REDACTED***";

pub async fn raw_io_logger(
    State(app_state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !app_state.config.enable_raw_logging {
        return next.run(request).await;
    }

    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let log_dir = PathBuf::from("logs")
        .join("raw_io")
        .join(format!("{timestamp}_{request_id}"));
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let headers = sanitize_headers(request.headers());

    let (parts, body) = request.into_parts();
    let body_bytes = match to_bytes(body, app_state.config.max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(error = %error, "failed to read request body for raw I/O logging");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    let request_body = bytes_to_json_value(&body_bytes);
    let streaming = request_body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = request_body
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let provider = model
        .as_deref()
        .and_then(|model| app_state.registry.resolve_provider_by_model(model))
        .map(ToOwned::to_owned);

    spawn_write_json(
        log_dir.clone(),
        "request.json",
        json!({
            "request_id": request_id,
            "timestamp_utc": utc_timestamp(),
            "method": method,
            "uri": uri,
            "headers": headers,
            "body": request_body,
        }),
    );

    let request = Request::from_parts(parts, Body::from(body_bytes));
    let response = next.run(request).await;
    let status = response.status();
    let response_headers = sanitize_headers(response.headers());
    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    response_with_logging(
        response,
        StreamLogContext {
            log_dir,
            request_id,
            started,
            status,
            headers: response_headers,
            provider,
            model,
            streaming,
            log_chunks: is_sse,
        },
    )
}

struct FinalLogContext {
    log_dir: PathBuf,
    request_id: String,
    duration_ms: u128,
    status: StatusCode,
    headers: Value,
    provider: Option<String>,
    model: Option<String>,
    streaming: bool,
}

struct StreamLogContext {
    log_dir: PathBuf,
    request_id: String,
    started: Instant,
    status: StatusCode,
    headers: Value,
    provider: Option<String>,
    model: Option<String>,
    streaming: bool,
    log_chunks: bool,
}

fn response_with_logging(response: Response, context: StreamLogContext) -> Response {
    let (parts, body) = response.into_parts();
    let stream = body.into_data_stream();
    const MAX_ACCUMULATED: usize = 10_000_000;
    let stream = futures::stream::unfold(
        (stream, Vec::new(), Some(context)),
        |(mut stream, mut accumulated, context)| async move {
            match stream.next().await {
                Some(Ok(bytes)) => {
                    if accumulated.len() < MAX_ACCUMULATED {
                        accumulated.extend_from_slice(&bytes);
                    }
                    if let Some(context) = &context
                        && context.log_chunks
                    {
                        spawn_write_jsonl(
                            context.log_dir.clone(),
                            "streaming_chunks.jsonl",
                            json!({
                                "timestamp_utc": utc_timestamp(),
                                "chunk": bytes_to_json_value(&bytes),
                            }),
                        );
                    }
                    Some((Ok(bytes), (stream, accumulated, context)))
                }
                Some(Err(error)) => Some((Err(error), (stream, accumulated, context))),
                None => {
                    if let Some(context) = context {
                        let body = bytes_to_json_value(&Bytes::from(accumulated));
                        spawn_final_logs(
                            FinalLogContext {
                                log_dir: context.log_dir,
                                request_id: context.request_id,
                                duration_ms: context.started.elapsed().as_millis(),
                                status: context.status,
                                headers: context.headers,
                                provider: context.provider,
                                model: context.model,
                                streaming: context.streaming,
                            },
                            body,
                        );
                    }
                    None
                }
            }
        },
    );

    Response::from_parts(parts, Body::from_stream(stream))
}

fn spawn_final_logs(context: FinalLogContext, body: Value) {
    let timestamp_utc = utc_timestamp();
    let final_response = json!({
        "request_id": context.request_id,
        "timestamp_utc": timestamp_utc,
        "status_code": context.status.as_u16(),
        "duration_ms": context.duration_ms,
        "headers": context.headers,
        "body": body,
    });
    let metadata = metadata_json(&context, &final_response, timestamp_utc);
    spawn_write_json(
        context.log_dir.clone(),
        "final_response.json",
        final_response,
    );
    spawn_write_json(context.log_dir, "metadata.json", metadata);
}

fn metadata_json(
    context: &FinalLogContext,
    final_response: &Value,
    timestamp_utc: String,
) -> Value {
    let body = final_response.get("body").unwrap_or(&Value::Null);
    let usage = body.get("usage").unwrap_or(&Value::Null);
    let finish_reason = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .cloned()
        .unwrap_or(Value::String("N/A".to_owned()));
    let response_model = body
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| context.model.clone())
        .unwrap_or_else(|| "N/A".to_owned());

    json!({
        "request_id": context.request_id,
        "timestamp_utc": timestamp_utc,
        "duration_ms": context.duration_ms,
        "status_code": context.status.as_u16(),
        "provider": context.provider,
        "model": response_model,
        "streaming": context.streaming,
        "usage": {
            "prompt_tokens": usage.get("prompt_tokens").cloned().unwrap_or(Value::Null),
            "completion_tokens": usage.get("completion_tokens").cloned().unwrap_or(Value::Null),
            "total_tokens": usage.get("total_tokens").cloned().unwrap_or(Value::Null),
        },
        "finish_reason": finish_reason,
    })
}

fn sanitize_headers(headers: &HeaderMap) -> Value {
    let mut map = Map::new();
    for (name, value) in headers {
        let key = name.as_str();
        let normalized = key.to_ascii_lowercase().replace('-', "_");
        let value = if is_sensitive_header(&normalized) {
            REDACTED.to_owned()
        } else {
            value.to_str().unwrap_or("<non-utf8>").to_owned()
        };
        map.insert(key.to_owned(), Value::String(value));
    }
    Value::Object(map)
}

fn is_sensitive_header(normalized: &str) -> bool {
    normalized == "authorization"
        || normalized == "proxy_authorization"
        || ["api_key", "token", "secret", "password", "cookie"]
            .iter()
            .any(|marker| normalized.contains(marker))
}

fn bytes_to_json_value(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

fn spawn_write_json(log_dir: PathBuf, filename: &'static str, value: Value) {
    tokio::spawn(async move {
        if let Err(error) = write_json(log_dir, filename, value).await {
            tracing::warn!(error = %error, filename, "failed to write raw I/O log");
        }
    });
}

fn spawn_write_jsonl(log_dir: PathBuf, filename: &'static str, value: Value) {
    tokio::spawn(async move {
        if let Err(error) = write_jsonl(log_dir, filename, value).await {
            tracing::warn!(error = %error, filename, "failed to write raw I/O stream log");
        }
    });
}

async fn write_json(log_dir: PathBuf, filename: &str, value: Value) -> std::io::Result<()> {
    tokio::fs::create_dir_all(&log_dir).await?;
    let content = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_owned());
    tokio::fs::write(log_dir.join(filename), content).await
}

async fn write_jsonl(log_dir: PathBuf, filename: &str, value: Value) -> std::io::Result<()> {
    tokio::fs::create_dir_all(&log_dir).await?;
    let mut content = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
    content.push('\n');
    let path = log_dir.join(filename);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(content.as_bytes()).await
}

fn utc_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
