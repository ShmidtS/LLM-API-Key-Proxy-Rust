use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use proxy_app::state::AppState;
use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

async fn upstream_server(content_type: &'static str, body: String) -> String {
    upstream_server_with_status(StatusCode::OK, content_type, body, None).await
}

async fn upstream_server_with_status(
    status: StatusCode,
    content_type: &'static str,
    body: String,
    captured_request: Option<Arc<Mutex<String>>>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = [0; 8192];
            let Ok(bytes_read) = socket.read(&mut buffer).await else {
                continue;
            };
            if let Some(captured_request) = &captured_request {
                *captured_request.lock().unwrap() =
                    String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
            }
            let reason = status.canonical_reason().unwrap_or("OK");
            let response = format!(
                "HTTP/1.1 {} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\r\n{}",
                status.as_u16(),
                reason,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    format!("http://{addr}/v1")
}

fn test_state(base_url: String) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
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
    });

    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec!["test-key".to_owned()], 10);
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
    state
}

async fn post_response(state: AppState, body: Value) -> axum::response::Response {
    proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn responses_non_stream_returns_response_shape() {
    let upstream_body = json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 123,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    })
    .to_string();
    let state = test_state(upstream_server("application/json", upstream_body).await);

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": "say hello"
        }),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert!(body["output"].as_array().unwrap().len() == 1);
    assert_eq!(body["usage"]["total_tokens"], 5);
}

#[tokio::test]
async fn responses_stream_converts_chat_sse_events() {
    let upstream_body = concat!(
        "data: {\"id\":\"chatcmpl_stream\",\"object\":\"chat.completion.chunk\",\"created\":123,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl_stream\",\"object\":\"chat.completion.chunk\",\"created\":123,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl_stream\",\"object\":\"chat.completion.chunk\",\"created\":123,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .to_owned();
    let state = test_state(upstream_server("text/event-stream", upstream_body).await);

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": "say hi",
            "stream": true
        }),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: response.created"));
    assert!(body.contains("event: response.output_item.added"));
    assert!(body.contains("event: response.output_text.delta"));
    assert!(body.contains("event: response.completed"));
}

#[tokio::test]
async fn responses_tool_call_returns_output_item() {
    let upstream_body = json!({
        "id": "chatcmpl_tool",
        "object": "chat.completion",
        "created": 123,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 6, "completion_tokens": 4, "total_tokens": 10}
    })
    .to_string();
    let state = test_state(upstream_server("application/json", upstream_body).await);

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": "lookup rust",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "lookup things",
                    "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
                }
            }]
        }),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let output = body["output"].as_array().unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(output.iter().any(|item| item["type"] == "function_call"));
}

#[tokio::test]
async fn responses_non_stream_preserves_upstream_error_response() {
    let upstream_body = json!({
        "error": {
            "message": "bad upstream request",
            "type": "invalid_request_error",
            "code": "bad_request"
        }
    })
    .to_string();
    let state = test_state(
        upstream_server_with_status(
            StatusCode::BAD_REQUEST,
            "application/json",
            upstream_body.clone(),
            None,
        )
        .await,
    );

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": "bad request"
        }),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "bad upstream request");
}

#[tokio::test]
async fn responses_object_tool_choice_forwards_function_choice() {
    let upstream_body = json!({
        "id": "chatcmpl_choice",
        "object": "chat.completion",
        "created": 123,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    })
    .to_string();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server_with_status(
            StatusCode::OK,
            "application/json",
            upstream_body,
            Some(captured_request.clone()),
        )
        .await,
    );

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": "use lookup",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "parameters": {"type": "object", "properties": {}}
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "lookup"}
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let raw_request = captured_request.lock().unwrap().clone();
    let (_, body) = raw_request.split_once("\r\n\r\n").unwrap();
    let upstream_json: Value = serde_json::from_str(body).unwrap();
    assert_eq!(upstream_json["tool_choice"]["type"], "function");
    assert_eq!(upstream_json["tool_choice"]["function"]["name"], "lookup");
}

#[tokio::test]
async fn responses_function_call_continuation_forwards_chat_messages() {
    let upstream_body = json!({
        "id": "chatcmpl_continue",
        "object": "chat.completion",
        "created": 123,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "done"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10}
    })
    .to_string();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server_with_status(
            StatusCode::OK,
            "application/json",
            upstream_body,
            Some(captured_request.clone()),
        )
        .await,
    );

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": [
                {
                    "type": "function_call",
                    "id": "fc_123",
                    "call_id": "call_123",
                    "name": "lookup",
                    "arguments": "{\"q\":\"rust\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_123",
                    "output": "Rust is a language"
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let raw_request = captured_request.lock().unwrap().clone();
    let (_, body) = raw_request.split_once("\r\n\r\n").unwrap();
    let upstream_json: Value = serde_json::from_str(body).unwrap();
    let messages = upstream_json["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"][0]["id"], "call_123");
    assert_eq!(messages[0]["tool_calls"][0]["function"]["name"], "lookup");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "call_123");
    assert_eq!(messages[1]["content"], "Rust is a language");
}

#[tokio::test]
async fn responses_rejects_unsupported_tools() {
    let state = test_state(upstream_server("application/json", "{}".to_owned()).await);

    let response = post_response(
        state,
        json!({
            "model": "gpt-4o-mini",
            "input": "search",
            "tools": [{"type": "web_search_preview"}]
        }),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unsupported tool type")
    );
}
