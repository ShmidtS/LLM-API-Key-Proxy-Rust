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
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Duration, sleep},
};
use tower::ServiceExt;

async fn sse_stream_server(chunks: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer).await;
        let response_headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        if socket.write_all(response_headers.as_bytes()).await.is_err() {
            return;
        }
        for chunk in chunks {
            let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
            if socket.write_all(frame.as_bytes()).await.is_err() {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
    });

    format!("http://{addr}/v1")
}

fn provider_test_state(provider: &str, base_url: String) -> AppState {
    let registry = Arc::new(ProviderRegistry::default());
    let endpoint = if provider == "anthropic" {
        "/messages"
    } else {
        "/responses"
    };
    registry.register(ProviderDefinition {
        id: provider.to_owned(),
        display_name: provider.to_owned(),
        base_url,
        auth_type: AuthType::ApiKey,
        model_patterns: vec![format!(r"^{provider}/.*")],
            compiled_patterns: Vec::new(),
        endpoints: vec![endpoint.to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 1,
        timeout_secs: 30,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    let credentials = CredentialManager::new();
    credentials.register_keys(provider.to_owned(), vec!["test-key".to_owned()], 10);
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

fn anthropic_test_state(base_url: String) -> AppState {
    provider_test_state("anthropic", base_url)
}

fn openai_stream_payloads(body: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(body)
        .split("\n\n")
        .filter_map(|event| event.strip_prefix("data: ").map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn chat_completions_stream_true_returns_valid_http_response() {
    let creds = CredentialManager::new();
    creds.register_keys("openai".to_owned(), vec!["test-dummy-key".to_owned()], 10);
    let registry = Arc::new(ProviderRegistry::new());
    let client = RotatorClient::new(
        creds,
        HttpClientPool::new(30),
        registry.clone(),
        Arc::new(rotator::RateLimiterRegistry::new()),
        Arc::new(rotator::CooldownManager::new()),
        Arc::new(rotator::CircuitBreakerRegistry::new()),
        None,
        3,
    );
    let mut state = AppState::with_parts(client, registry);
    state.config.api_keys = vec!["test-proxy-token".to_owned()];

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "gpt-4o-mini",
                        "messages": [
                            {"role": "user", "content": "hello"}
                        ],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // With invalid credentials, upstream forwarding fails, proving the proxy path is active
    // (previously this returned a hardcoded 200 SSE placeholder)
    assert_ne!(response.status(), StatusCode::OK);
    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "expected upstream failure, got {:?}",
        response.status()
    );
}

#[tokio::test]
async fn anthropic_streaming_route_handles_split_chunks_and_done() {
    let base_url = sse_stream_server(vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-test\",\"role\":\"assistant\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"He",
        "l\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        "data: [DONE]\n\n",
    ])
    .await;
    let state = anthropic_test_state(base_url);

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "claude-test",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 32,
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payloads = openai_stream_payloads(&body);

    assert_eq!(payloads.last().map(String::as_str), Some("[DONE]"));
    assert_eq!(
        payloads
            .iter()
            .filter(|payload| payload.as_str() == "[DONE]")
            .count(),
        1
    );

    let chunks: Vec<Value> = payloads[..payloads.len() - 1]
        .iter()
        .map(|payload| serde_json::from_str(payload).unwrap())
        .collect();
    let text: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();

    assert_eq!(text, "Hello");
    assert_eq!(
        chunks[0]["choices"][0]["delta"],
        json!({"role": "assistant"})
    );
    assert_eq!(chunks[3]["choices"][0]["finish_reason"], json!("stop"));
}

#[tokio::test]
async fn anthropic_streaming_route_ignores_malformed_chunks_without_crashing() {
    let base_url = sse_stream_server(vec![
        "event: content_block_delta\ndata: not-json\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "data: [DONE]\n\n",
    ])
    .await;
    let state = anthropic_test_state(base_url);

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "claude-test",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 32,
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payloads = openai_stream_payloads(&body);

    assert_eq!(payloads.last().map(String::as_str), Some("[DONE]"));
    let chunks: Vec<Value> = payloads[..payloads.len() - 1]
        .iter()
        .map(|payload| serde_json::from_str(payload).unwrap())
        .collect();
    let text: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();

    assert_eq!(text, "ok");
}

#[tokio::test]
async fn elysiver_non_streaming_chat_aggregates_responses_sse() {
    let base_url = sse_stream_server(vec![
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"created_at\":123,\"status\":\"in_progress\",\"model\":\"gpt-5.5\",\"output\":[]}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hel\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"lo\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"created_at\":123,\"status\":\"completed\",\"model\":\"gpt-5.5\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}},\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}\n\n",
        "data: [DONE]\n\n",
    ])
    .await;
    let state = provider_test_state("elysiver", base_url);

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "elysiver/gpt-5.5",
                        "messages": [
                            {"role": "system", "content": "You are concise."},
                            {"role": "user", "content": "hello"}
                        ],
                        "max_tokens": 32,
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["object"], "chat.completion");
    assert_eq!(value["model"], "gpt-5.5");
    assert_eq!(value["choices"][0]["message"]["content"], "Hello");
    assert_eq!(value["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        value["usage"],
        json!({"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5})
    );
}

#[tokio::test]
async fn colin_streaming_chat_converts_responses_sse_to_chat_sse() {
    let base_url = sse_stream_server(vec![
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\",\"created_at\":456,\"status\":\"in_progress\",\"model\":\"gpt-5.4\",\"output\":[]}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"created_at\":456,\"status\":\"completed\",\"model\":\"gpt-5.4\",\"output\":[]}}\n\n",
        "data: [DONE]\n\n",
    ])
    .await;
    let state = provider_test_state("colin", base_url);

    let response = proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(
                    json!({
                        "model": "colin/gpt-5.4",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payloads = openai_stream_payloads(&body);

    assert_eq!(payloads.last().map(String::as_str), Some("[DONE]"));
    let chunks: Vec<Value> = payloads[..payloads.len() - 1]
        .iter()
        .map(|payload| serde_json::from_str(payload).unwrap())
        .collect();
    let text: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();

    assert_eq!(
        chunks[0]["choices"][0]["delta"],
        json!({"role": "assistant"})
    );
    assert_eq!(text, "OK");
    assert_eq!(
        chunks.last().unwrap()["choices"][0]["finish_reason"],
        json!("stop")
    );
}
