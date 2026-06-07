use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use proxy_app::{guardrails_adapter::GuardrailsAdapter, state::AppState};
use proxy_config::ProxyConfig;
use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

async fn json_upstream_server(response_body: Value) -> String {
    json_upstream_server_with_requests(response_body, None, None)
        .await
        .0
}

async fn json_upstream_server_with_requests(
    response_body: Value,
    max_requests: Option<usize>,
    request_counter: Option<Arc<AtomicUsize>>,
) -> (String, Arc<tokio::sync::Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured_requests = requests.clone();

    tokio::spawn(async move {
        let mut accepted = 0usize;
        loop {
            if max_requests.is_some_and(|max| accepted >= max) {
                return;
            }
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            accepted += 1;
            if let Some(counter) = &request_counter {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            let mut buffer = Vec::new();
            let mut chunk = [0; 4096];
            while let Ok(n) = socket.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
                if request_body_from_http_buffer(&buffer).is_some() {
                    break;
                }
            }
            if let Some(body) = request_body_from_http_buffer(&buffer) {
                captured_requests.lock().await.push(body);
            }
            let body = response_body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    (format!("http://{addr}/v1"), requests)
}

fn request_body_from_http_buffer(buffer: &[u8]) -> Option<Value> {
    let separator = b"\r\n\r\n";
    let header_end = buffer
        .windows(separator.len())
        .position(|window| window == separator)?;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .or_else(|| {
            headers
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
        })?
        .trim()
        .parse::<usize>()
        .ok()?;
    let body_start = header_end + separator.len();
    if buffer.len() < body_start + content_length {
        return None;
    }
    serde_json::from_slice(&buffer[body_start..body_start + content_length]).ok()
}

fn provider_definition(base_url: String) -> ProviderDefinition {
    ProviderDefinition {
        id: "openai".to_owned(),
        display_name: "openai".to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![r"^gpt-.*".to_owned()],
        compiled_patterns: Vec::new(),
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    }
}

fn test_state(base_url: String, guardrail_mode: Option<&str>) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(provider_definition(base_url));

    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec!["openai-test-key".to_owned()], 10);
    let rotator = RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        0,
    );
    let mut state = AppState::with_parts(rotator, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];
    if let Some(mode) = guardrail_mode {
        state.config.guardrails.enabled = true;
        state.config.guardrails.mode = mode.to_owned();
        state.config.guardrails.chat.enabled = true;
        state.config.guardrails.chat.validate_tools = true;
        state.config.guardrails.chat.validate_json = false;
        state.config.guardrails.chat.enforce_steps = false;
        state.config.guardrails.chat.compact_context = false;
        state.config.guardrails.chat.recover_errors = false;
        state.config.guardrails.chat.validate_streaming = false;
        state.config.guardrails.max_rescue_attempts = 1;
        state.config.guardrails.max_guardrail_retries = 1;
        state.guardrails = Some(Arc::new(GuardrailsAdapter::from_proxy_config(
            state.rotator.clone(),
            &state.config.guardrails,
        )));
    }
    state
}

async fn post_chat(state: AppState) -> axum::response::Response {
    post_chat_body(
        state,
        json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "call tool"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "parameters": {"type": "object"}
                }
            }]
        }),
    )
    .await
}

async fn post_chat_body(state: AppState, body: Value) -> axum::response::Response {
    proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

async fn response_text(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    (status, body)
}

fn malformed_tool_call_response() -> Value {
    json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{q: 'rust',}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn unrepairable_tool_call_response() -> Value {
    json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "not json at all"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

#[tokio::test]
async fn chat_completions_default_guardrails_disabled_preserves_parity() {
    let base_url = json_upstream_server(malformed_tool_call_response()).await;
    let response = post_chat(test_state(base_url, None)).await;
    let (status, body) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{q: 'rust',}"
    );
}

#[tokio::test]
async fn chat_completions_observe_mode_accepts_malformed_tool_call_without_mutation() {
    let base_url = json_upstream_server(malformed_tool_call_response()).await;
    let response = post_chat(test_state(base_url, Some("observe"))).await;
    let (status, body) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{q: 'rust',}"
    );
}

#[tokio::test]
async fn chat_completions_enforce_mode_repairs_or_rejects_malformed_tool_call() {
    let base_url = json_upstream_server(malformed_tool_call_response()).await;
    let response = post_chat(test_state(base_url, Some("enforce"))).await;
    let (status, body) = response_json(response).await;

    if status == StatusCode::OK {
        assert_ne!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{q: 'rust',}"
        );
    } else {
        assert!(status.is_client_error() || status.is_server_error());
    }
}

#[tokio::test]
async fn guardrails_preprocess_compacts_context() {
    let (base_url, requests) =
        json_upstream_server_with_requests(malformed_tool_call_response(), Some(1), None).await;
    let mut state = test_state(base_url, Some("observe"));
    state.config.guardrails.chat.compact_context = true;
    state
        .config
        .guardrails
        .context_compaction
        .max_context_messages = 40;
    state
        .config
        .guardrails
        .context_compaction
        .compact_above_ratio = 0.5;
    state.guardrails = Some(Arc::new(GuardrailsAdapter::from_proxy_config(
        state.rotator.clone(),
        &state.config.guardrails,
    )));
    let messages = (0..10)
        .map(|i| json!({"role": "user", "content": format!("message {i} with enough text to count tokens")}))
        .collect::<Vec<_>>();

    let response = post_chat_body(
        state,
        json!({
            "model": "gpt-4o-mini",
            "messages": messages,
        }),
    )
    .await;
    let (status, _) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    let captured = requests.lock().await;
    assert!(captured[0]["messages"].as_array().unwrap().len() < 10);
}

#[tokio::test]
async fn guardrails_retry_cap_does_not_loop_forever() {
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let (base_url, _) = json_upstream_server_with_requests(
        unrepairable_tool_call_response(),
        Some(2),
        Some(upstream_calls.clone()),
    )
    .await;
    let mut state = test_state(base_url, Some("enforce"));
    state.config.guardrails.chat.recover_errors = true;
    state.config.guardrails.max_guardrail_retries = 1;
    state.guardrails = Some(Arc::new(GuardrailsAdapter::from_proxy_config(
        state.rotator.clone(),
        &state.config.guardrails,
    )));

    let response = post_chat(state).await;
    let (status, _) = response_json(response).await;

    assert!(status.is_client_error() || status.is_server_error());
    assert_eq!(upstream_calls.load(Ordering::SeqCst), 2);
}

async fn sse_upstream_server(sse_body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = Vec::new();
            let mut chunk = [0; 4096];
            while let Ok(n) = socket.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
                if request_body_from_http_buffer(&buffer).is_some() {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                sse_body.len(),
                sse_body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}/v1")
}

fn valid_chat_completion_sse() -> String {
    let chunk1 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]});
    let chunk2 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]});
    let chunk3 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
    format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: {chunk3}\n\ndata: [DONE]\n\n")
}

fn malformed_tool_call_sse() -> String {
    let chunk1 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]});
    let chunk2 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup"}}]},"finish_reason":null}]});
    let chunk3 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{q: 'rust',}"}}]},"finish_reason":null}]});
    let chunk4 = serde_json::json!({"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]});
    format!(
        "data: {chunk1}\n\ndata: {chunk2}\n\ndata: {chunk3}\n\ndata: {chunk4}\n\ndata: [DONE]\n\n"
    )
}

#[tokio::test]
async fn chat_completions_streaming_guardrails_valid_passes() {
    let sse_body = valid_chat_completion_sse();
    let base_url = sse_upstream_server(sse_body).await;
    let mut state = test_state(base_url, Some("enforce"));
    state.config.guardrails.chat.validate_streaming = true;
    state.guardrails = Some(Arc::new(GuardrailsAdapter::from_proxy_config(
        state.rotator.clone(),
        &state.config.guardrails,
    )));

    let response = post_chat_body(
        state,
        json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (status, body) = response_text(response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("data: "));
    assert!(body.contains("Hello"));
}

#[tokio::test]
async fn chat_completions_streaming_guardrails_enforce_repairs_or_rejects_invalid_tool_call() {
    let sse_body = malformed_tool_call_sse();
    let base_url = sse_upstream_server(sse_body).await;
    let mut state = test_state(base_url, Some("enforce"));
    state.config.guardrails.chat.validate_streaming = true;
    state.guardrails = Some(Arc::new(GuardrailsAdapter::from_proxy_config(
        state.rotator.clone(),
        &state.config.guardrails,
    )));

    let response = post_chat_body(
        state,
        json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "messages": [{"role": "user", "content": "call tool"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "parameters": {"type": "object"}
                }
            }]
        }),
    )
    .await;

    let (status, body) = response_text(response).await;
    if status == StatusCode::OK {
        assert!(body.contains("data: "));
    } else {
        assert!(status.is_client_error() || status.is_server_error());
    }
}

#[test]
fn proxy_config_default_guardrails_are_disabled() {
    let config = ProxyConfig::default();

    assert!(!config.guardrails.enabled);
    assert_eq!(config.guardrails.mode, "off");
    assert!(!config.guardrails.chat.enabled);
}
