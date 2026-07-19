use axum::{
    body::Body,
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

async fn upstream_server(
    captured_request: Arc<Mutex<String>>,
    content_type: &'static str,
    body: String,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0; 8192];
        let bytes_read = socket.read(&mut buffer).await.unwrap_or(0);
        *captured_request.lock().unwrap() =
            String::from_utf8_lossy(&buffer[..bytes_read]).to_string();

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
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
        model_patterns: vec![
            r"^(openai/)?gpt[-/].*".to_owned(),
            r"^(openai/)?o4.*".to_owned(),
        ],
        compiled_patterns: Vec::new(),
        endpoints: vec!["/chat/completions".to_owned(), "/responses".to_owned()],
        features: vec!["chat".to_owned(), "responses".to_owned()],
        model_count: 2,
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

async fn post_json(state: AppState, uri: &str, body: Value) -> axum::response::Response {
    proxy_app::build_app_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", "test-proxy-token")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn captured_path_and_body(captured_request: &Arc<Mutex<String>>) -> (String, Value) {
    let raw_request = captured_request.lock().unwrap().clone();
    let (headers, body) = raw_request.split_once("\r\n\r\n").unwrap();
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap()
        .to_owned();
    (path, serde_json::from_str(body).unwrap())
}

fn responses_sse_body() -> String {
    concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test\",\"created_at\":123,\"model\":\"gpt-5.5\",\"output\":[{\"content\":[{\"text\":\"ok\"}]}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_owned()
}

fn chat_completion_body() -> String {
    json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 123,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string()
}

#[tokio::test]
async fn chat_openai_prefixed_gpt5_routes_to_responses_with_input() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server(
            captured_request.clone(),
            "text/event-stream",
            responses_sse_body(),
        )
        .await,
    );

    let response = post_json(
        state,
        "/v1/chat/completions",
        json!({
            "model": "openai/gpt-5.5",
            "messages": [
                {"role": "system", "content": "You are concise."},
                {"role": "user", "content": "hello"}
            ],
            "stream": false,
            "stop": ["done"],
            "presence_penalty": 0.1,
            "frequency_penalty": 0.2,
            "user": "test-user",
            "response_format": {"type": "json_object"}
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (path, upstream_json) = captured_path_and_body(&captured_request);
    assert_eq!(path, "/v1/responses");
    assert_eq!(upstream_json["model"], "gpt-5.5");
    assert_eq!(upstream_json["instructions"], "You are concise.");
    assert_eq!(
        upstream_json["input"],
        json!([{ "type": "message", "role": "user", "content": "hello" }])
    );
    assert_eq!(upstream_json["stream"], false);
    assert!(upstream_json.get("messages").is_none());
    assert!(upstream_json.get("stop").is_none());
    assert!(upstream_json.get("presence_penalty").is_none());
    assert!(upstream_json.get("frequency_penalty").is_none());
    assert!(upstream_json.get("user").is_none());
    assert!(upstream_json.get("response_format").is_none());
}

#[tokio::test]
async fn chat_openai_bare_gpt5_routes_to_responses_with_input() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server(
            captured_request.clone(),
            "text/event-stream",
            responses_sse_body(),
        )
        .await,
    );

    let response = post_json(
        state,
        "/v1/chat/completions",
        json!({
            "model": "gpt-5.5",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (path, upstream_json) = captured_path_and_body(&captured_request);
    assert_eq!(path, "/v1/responses");
    assert_eq!(upstream_json["model"], "gpt-5.5");
    assert!(upstream_json.get("messages").is_none());
    assert!(upstream_json.get("input").is_some());
}

#[tokio::test]
async fn chat_openai_gpt5_converts_tool_calls_and_outputs() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server(
            captured_request.clone(),
            "text/event-stream",
            responses_sse_body(),
        )
        .await,
    );

    let response = post_json(
        state,
        "/v1/chat/completions",
        json!({
            "model": "openai/gpt-5.5",
            "messages": [
                {"role": "user", "content": "weather"},
                {
                    "role": "assistant",
                    "content": "checking",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object"}
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "get_weather", "parameters": {}}
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (path, upstream_json) = captured_path_and_body(&captured_request);
    assert_eq!(path, "/v1/responses");
    assert_eq!(
        upstream_json["input"],
        json!([
            {"type": "message", "role": "user", "content": "weather"},
            {"type": "message", "role": "assistant", "content": "checking"},
            {"type": "function_call", "id": "fc_call_1", "call_id": "fc_call_1", "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"},
            {"type": "function_call_output", "call_id": "fc_call_1", "output": "sunny"}
        ])
    );
    assert_eq!(upstream_json["tools"][0]["type"], "function");
    assert_eq!(upstream_json["tools"][0]["name"], "get_weather");
    assert_eq!(upstream_json["tool_choice"]["type"], "function");
    assert_eq!(upstream_json["tool_choice"]["name"], "get_weather");
}

#[tokio::test]
async fn responses_openai_prefixed_gpt5_forwards_native_body() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let native_response = json!({
        "id": "resp_test",
        "object": "response",
        "created_at": 123,
        "model": "gpt-5.5",
        "output": []
    })
    .to_string();
    let state = test_state(
        upstream_server(
            captured_request.clone(),
            "application/json",
            native_response,
        )
        .await,
    );

    let response = post_json(
        state,
        "/v1/responses",
        json!({
            "model": "openai/gpt-5.5",
            "input": "say hello",
            "temperature": 0.7,
            "stream": false
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (path, upstream_json) = captured_path_and_body(&captured_request);
    assert_eq!(path, "/v1/responses");
    assert_eq!(upstream_json["model"], "gpt-5.5");
    assert_eq!(upstream_json["input"], "say hello");
    assert!((upstream_json["temperature"].as_f64().unwrap() - 0.7).abs() < 0.00001);
    assert_eq!(upstream_json["stream"], false);
    assert!(upstream_json.get("messages").is_none());
}

#[tokio::test]
async fn responses_openai_gpt4o_uses_chat_emulation() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server(
            captured_request.clone(),
            "application/json",
            chat_completion_body(),
        )
        .await,
    );

    let response = post_json(
        state,
        "/v1/responses",
        json!({
            "model": "openai/gpt-4o",
            "input": "say hello"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (path, upstream_json) = captured_path_and_body(&captured_request);
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(upstream_json["model"], "gpt-4o");
    assert!(upstream_json.get("messages").is_some());
    assert!(upstream_json.get("input").is_none());
}

#[tokio::test]
async fn chat_openai_gpt4o_uses_chat_completions_with_messages() {
    let captured_request = Arc::new(Mutex::new(String::new()));
    let state = test_state(
        upstream_server(
            captured_request.clone(),
            "application/json",
            chat_completion_body(),
        )
        .await,
    );

    let response = post_json(
        state,
        "/v1/chat/completions",
        json!({
            "model": "openai/gpt-4o",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (path, upstream_json) = captured_path_and_body(&captured_request);
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(upstream_json["model"], "gpt-4o");
    assert!(upstream_json.get("messages").is_some());
    assert!(upstream_json.get("input").is_none());
}
