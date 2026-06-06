use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client_for_openai(server: &MockServer) -> RotatorClient {
    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: "openai".to_owned(),
        display_name: "OpenAI".to_owned(),
        base_url: format!("{}/v1", server.uri()),
        auth_type: AuthType::Bearer,
        model_patterns: vec![
            r"^(gpt|o1|o3|o4)([-/].*)?$".to_owned(),
            r"^openai/.*".to_owned(),
        ],
        compiled_patterns: vec![],
        endpoints: vec!["/chat/completions".to_owned(), "/responses".to_owned()],
        features: vec!["chat".to_owned(), "streaming".to_owned()],
        model_count: 2,
        timeout_secs: 60,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec!["test-key".to_owned()], 1);

    RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry,
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        0,
    )
}

async fn assert_openai_request(model: &str, expected_path: &str) -> Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(expected_path))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for_openai(&server).await;
    let response = client
        .request(
            "openai",
            "chat/completions",
            json!({
                "model": model,
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 32,
                "stream": false
            }),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    serde_json::from_slice(&requests[0].body).unwrap()
}

#[tokio::test]
async fn openai_prefixed_gpt5_chat_input_uses_responses_endpoint_and_body() {
    let body = assert_openai_request("openai/gpt-5-mini", "/v1/responses").await;

    assert_eq!(body["model"], "gpt-5-mini");
    assert!(body.get("messages").is_none());
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"], "hello");
    assert_eq!(body["input"][0]["type"], "message");
    assert_eq!(body["max_output_tokens"], 32);
    assert!(body.get("max_tokens").is_none());
}

#[tokio::test]
async fn openai_bare_gpt5_chat_input_uses_responses_endpoint_and_body() {
    let body = assert_openai_request("gpt-5", "/v1/responses").await;

    assert_eq!(body["model"], "gpt-5");
    assert!(body.get("messages").is_none());
    assert_eq!(body["input"][0]["content"], "hello");
    assert_eq!(body["max_output_tokens"], 32);
}

#[tokio::test]
async fn openai_gpt4o_stays_on_chat_completions_with_chat_body() {
    let body = assert_openai_request("openai/gpt-4o", "/v1/chat/completions").await;

    // upstream OpenAI принимает чистое имя модели: префикс провайдера зачищается
    // (паритет с Python LiteLLM, который шлёт на upstream имя без префикса).
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hello");
    assert_eq!(body["max_tokens"], 32);
    assert!(body.get("input").is_none());
    assert!(body.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn openai_o4_mini_chat_input_uses_responses_endpoint_and_body() {
    let body = assert_openai_request("o4-mini", "/v1/responses").await;

    assert_eq!(body["model"], "o4-mini");
    assert!(body.get("messages").is_none());
    assert_eq!(body["input"][0]["content"], "hello");
    assert_eq!(body["max_output_tokens"], 32);
}

#[tokio::test]
#[ignore = "live smoke requires OPENAI_API_KEY and should be run explicitly"]
async fn live_openai_responses_smoke() {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };

    let mut registry = ProviderRegistry::new();
    registry.load_from_env();
    let credentials = CredentialManager::new();
    credentials.register_keys("openai".to_owned(), vec![key], 1);
    let client = RotatorClient::new(
        credentials,
        HttpClientPool::new(60),
        Arc::new(registry),
        Arc::new(RateLimiterRegistry::new()),
        Arc::new(CooldownManager::new()),
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        0,
    );

    let response = client
        .request(
            "openai",
            "chat/completions",
            json!({
                "model": "gpt-5-mini",
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 16,
                "stream": false
            }),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}

#[test]
fn chat_tools_converted_to_flat_responses_format() {
    use models::chat::{ChatCompletionRequest, FunctionDefinition, ToolDefinition};
    use serde_json::json;

    let chat_req = ChatCompletionRequest {
        model: "openai/gpt-5.3-codex".to_owned(),
        messages: vec![models::chat::ChatMessage {
            role: "user".to_owned(),
            content: Some(models::chat::ChatMessageContent::Text("hello".to_owned())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        stream: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        tools: Some(vec![ToolDefinition {
            r#type: "function".to_owned(),
            function: FunctionDefinition {
                name: "get_weather".to_owned(),
                description: Some("Get weather".to_owned()),
                parameters: json!({"type": "object"}),
            },
        }]),
        tool_choice: None,
        user: None,
        response_format: None,
        extra: std::collections::HashMap::new(),
    };

    let responses_req = rotator::openai_responses::chat_request_to_responses_request(&chat_req)
        .unwrap();
    let body = serde_json::to_value(responses_req).unwrap();

    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "get_weather");
    assert_eq!(tools[0]["description"], "Get weather");
    assert_eq!(tools[0]["parameters"], json!({"type": "object"}));
    assert!(tools[0].get("function").is_none(), "tools must be flat, not nested under function");
}

#[test]
fn chat_tool_choice_converted_to_flat_responses_format() {
    use models::chat::{ChatCompletionRequest, FunctionDefinition, ToolChoice};
    use serde_json::json;

    let chat_req = ChatCompletionRequest {
        model: "openai/gpt-5.3-codex".to_owned(),
        messages: vec![models::chat::ChatMessage {
            role: "user".to_owned(),
            content: Some(models::chat::ChatMessageContent::Text("hello".to_owned())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        stream: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        tools: None,
        tool_choice: Some(ToolChoice::Object {
            r#type: "function".to_owned(),
            function: FunctionDefinition {
                name: "get_weather".to_owned(),
                description: None,
                parameters: json!({}),
            },
        }),
        user: None,
        response_format: None,
        extra: std::collections::HashMap::new(),
    };

    let responses_req = rotator::openai_responses::chat_request_to_responses_request(&chat_req)
        .unwrap();
    let body = serde_json::to_value(responses_req).unwrap();

    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["name"], "get_weather");
    assert!(body["tool_choice"].get("function").is_none(), "tool_choice must be flat, not nested under function");
}

#[test]
fn reasoning_models_drop_temperature_and_top_p() {
    use models::chat::ChatCompletionRequest;

    for model in ["openai/gpt-5.3-codex", "o4-mini"] {
        let chat_req = ChatCompletionRequest {
            model: model.to_owned(),
            messages: vec![models::chat::ChatMessage {
                role: "user".to_owned(),
                content: Some(models::chat::ChatMessageContent::Text("hello".to_owned())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: Some(0.7),
            max_tokens: None,
            top_p: Some(0.9),
            stream: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: None,
            user: None,
            response_format: None,
            extra: std::collections::HashMap::new(),
        };

        let responses_req = rotator::openai_responses::chat_request_to_responses_request(&chat_req)
            .unwrap();
        assert!(
            responses_req.temperature.is_none(),
            "model={model}: temperature must be removed for reasoning models"
        );
        assert!(
            responses_req.top_p.is_none(),
            "model={model}: top_p must be removed for reasoning models"
        );
    }
}

#[test]
fn non_reasoning_models_preserve_temperature_and_top_p() {
    use models::chat::ChatCompletionRequest;

    let chat_req = ChatCompletionRequest {
        model: "openai/gpt-4o".to_owned(),
        messages: vec![models::chat::ChatMessage {
            role: "user".to_owned(),
            content: Some(models::chat::ChatMessageContent::Text("hello".to_owned())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: Some(0.7),
        max_tokens: None,
        top_p: Some(0.9),
        stream: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        tools: None,
        tool_choice: None,
        user: None,
        response_format: None,
        extra: std::collections::HashMap::new(),
    };

    let responses_req = rotator::openai_responses::chat_request_to_responses_request(&chat_req)
        .unwrap();
    assert_eq!(responses_req.temperature, Some(0.7));
    assert_eq!(responses_req.top_p, Some(0.9));
}

#[test]
fn chat_blocks_converted_to_content_parts_array() {
    use models::chat::{ChatCompletionRequest, ChatMessageContent};
    use serde_json::json;

    let chat_req = ChatCompletionRequest {
        model: "openai/gpt-5-mini".to_owned(),
        messages: vec![models::chat::ChatMessage {
            role: "user".to_owned(),
            content: Some(ChatMessageContent::Blocks(vec![
                json!({"type": "text", "text": "hello"}),
            ])),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        stream: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        tools: None,
        tool_choice: None,
        user: None,
        response_format: None,
        extra: std::collections::HashMap::new(),
    };

    let responses_req = rotator::openai_responses::chat_request_to_responses_request(&chat_req)
        .unwrap();
    let body = serde_json::to_value(responses_req).unwrap();

    let input_content = &body["input"][0]["content"];
    assert!(input_content.is_array(), "blocks content must serialize as array, not string");
    let arr = input_content.as_array().unwrap();
    assert_eq!(arr[0]["type"], "input_text");
    assert_eq!(arr[0]["text"], "hello");
}
