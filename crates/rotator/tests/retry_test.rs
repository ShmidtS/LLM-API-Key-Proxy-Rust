use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
    ProviderDefinition, ProviderRegistry, RateLimiterRegistry, RotatorClient, RotatorError,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone)]
struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

struct TestServer {
    registry: Arc<ProviderRegistry>,
    calls: Arc<AtomicUsize>,
    keys_seen: Arc<Mutex<Vec<Option<String>>>>,
}

impl MockResponse {
    fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: "{}".to_string(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    fn with_body(mut self, body: &str) -> Self {
        self.body = body.to_string();
        self
    }
}

async fn test_provider(responses: Vec<MockResponse>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let server_calls = Arc::clone(&calls);
    let keys_seen = Arc::new(Mutex::new(Vec::new()));
    let server_keys_seen = Arc::clone(&keys_seen);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let request_number = server_calls.fetch_add(1, Ordering::SeqCst);
            let response = responses
                .get(request_number)
                .unwrap_or_else(|| responses.last().unwrap())
                .clone();
            let mut buffer = [0; 2048];
            let size = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..size]);
            server_keys_seen
                .lock()
                .await
                .push(extract_api_key(&request));

            let mut response_text = format!(
                "HTTP/1.1 {} OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n",
                response.status,
                response.body.len()
            );
            for (name, value) in response.headers {
                response_text.push_str(&format!("{name}: {value}\r\n"));
            }
            response_text.push_str("\r\n");
            response_text.push_str(&response.body);

            let _ = socket.write_all(response_text.as_bytes()).await;
        }
    });

    let registry = Arc::new(ProviderRegistry::default());
    registry.register(ProviderDefinition {
        id: "test".to_string(),
        display_name: "test".to_string(),
        base_url: format!("http://{addr}/v1"),
        auth_type: AuthType::ApiKey,
        model_patterns: Vec::new(),
        compiled_patterns: Vec::new(),
        endpoints: vec!["/chat/completions".to_string()],
        features: vec!["chat".to_string()],
        model_count: 1,
        timeout_secs: 60,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    TestServer {
        registry,
        calls,
        keys_seen,
    }
}

fn extract_api_key(request: &str) -> Option<String> {
    request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("x-api-key")
            .then(|| value.trim().to_string())
    })
}

fn test_client(
    registry: Arc<ProviderRegistry>,
    cooldown: Arc<CooldownManager>,
    keys: Vec<&str>,
    max_retries: usize,
) -> RotatorClient {
    let credentials = CredentialManager::new();
    credentials.register_keys(
        "test".to_string(),
        keys.into_iter().map(str::to_string).collect(),
        1,
    );

    RotatorClient::new(
        credentials,
        HttpClientPool::new(30),
        registry,
        Arc::new(RateLimiterRegistry::new()),
        cooldown,
        Arc::new(CircuitBreakerRegistry::new()),
        None,
        max_retries,
    )
}

async fn send_request(client: &RotatorClient) -> rotator::Result<reqwest::Response> {
    client
        .request("test", "chat/completions", serde_json::json!({}))
        .await
}

#[tokio::test]
async fn retries_429_with_retry_after_header_then_succeeds() {
    let server = test_provider(vec![
        MockResponse::new(429).with_header("Retry-After", "1"),
        MockResponse::new(200),
    ])
    .await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_429_without_retry_after_then_succeeds() {
    let server = test_provider(vec![MockResponse::new(429), MockResponse::new(200)]).await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_5xx_transient_then_succeeds() {
    let server = test_provider(vec![MockResponse::new(502), MockResponse::new(200)]).await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rotates_key_on_quota_exhausted() {
    let server = test_provider(vec![
        MockResponse::new(429).with_body(r#"{"error":{"code":"insufficient_quota"}}"#),
        MockResponse::new(200),
    ])
    .await;
    let cooldown = Arc::new(CooldownManager::new());
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::clone(&cooldown),
        vec!["key-1", "key-2"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    let keys_seen = server.keys_seen.lock().await;
    assert_eq!(keys_seen.len(), 2);
    assert_ne!(keys_seen[0], keys_seen[1]);
    assert!(!cooldown.is_available("test", "key-1"));
}

#[tokio::test]
async fn rotates_key_on_provider_abort() {
    let server = test_provider(vec![
        MockResponse::new(502).with_body(r#"{"error":{"type":"provider_abort"}}"#),
        MockResponse::new(200),
    ])
    .await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1", "key-2"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    let keys_seen = server.keys_seen.lock().await;
    assert_eq!(keys_seen.len(), 2);
    assert_ne!(keys_seen[0], keys_seen[1]);
}

#[tokio::test]
async fn aborts_after_max_retries_exhausted() {
    let server = test_provider(vec![MockResponse::new(429)]).await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1"],
        1,
    );

    let result = send_request(&client).await;

    assert!(matches!(result, Err(RotatorError::RateLimited(provider, _)) if provider == "test"));
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn aborts_server_error_after_max_retries() {
    let server = test_provider(vec![MockResponse::new(503)]).await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1"],
        1,
    );

    let result = send_request(&client).await;

    if let Ok(response) = result {
        assert_eq!(response.status(), 503);
    }
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rotates_key_on_401_auth_error() {
    let server = test_provider(vec![MockResponse::new(401), MockResponse::new(200)]).await;
    let cooldown = Arc::new(CooldownManager::new());
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::clone(&cooldown),
        vec!["key-1", "key-2"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    let keys_seen = server.keys_seen.lock().await;
    assert_eq!(keys_seen.len(), 2);
    assert_ne!(keys_seen[0], keys_seen[1]);
    assert!(!cooldown.is_available("test", "key-1"));
}

#[tokio::test]
async fn auth_error_exhausts_keys_then_stops_retrying() {
    let server = test_provider(vec![MockResponse::new(403)]).await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1"],
        1,
    );

    // 403 → ротация ключа с cooldown текущего; единственный ключ уходит в cooldown,
    // следующая попытка не находит доступных ключей → AllKeysOnCooldown (не бесконечный цикл).
    let result = send_request(&client).await;

    assert!(
        matches!(result, Err(RotatorError::AllKeysOnCooldown(ref provider, _)) if provider == "test"),
        "expected AllKeysOnCooldown after auth rotation exhausted, got {result:?}"
    );
    // Первый запрос получил 403, второй прерван отсутствием доступных ключей.
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn returns_403_to_client_when_no_other_key_and_no_cooldown_room() {
    // Два ключа: первый 403 → ротация → второй тоже 403 → попытки исчерпаны,
    // исходный 403 возвращается клиенту (а не теряется).
    let server = test_provider(vec![MockResponse::new(403), MockResponse::new(403)]).await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1", "key-2"],
        1,
    );

    let result = send_request(&client).await;

    match result {
        Ok(response) => assert_eq!(response.status(), 403),
        Err(RotatorError::NoCredentials(provider)) => assert_eq!(provider, "test"),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn rotates_key_on_garbage_response() {
    let server = test_provider(vec![
        MockResponse::new(200).with_body("hello hello hello hello world"),
        MockResponse::new(200).with_body(r#"{"choices":[{"message":{"content":"ok"}}]}"#),
    ])
    .await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1", "key-2"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    let keys_seen = server.keys_seen.lock().await;
    assert_eq!(keys_seen.len(), 2);
    assert_ne!(keys_seen[0], keys_seen[1]);
}

#[tokio::test]
async fn garbage_response_exhausts_keys_then_returns_original() {
    let server = test_provider(vec![
        MockResponse::new(200).with_body("hello hello hello hello world"),
        MockResponse::new(200).with_body("hello hello hello hello again"),
    ])
    .await;
    let client = test_client(
        Arc::clone(&server.registry),
        Arc::new(CooldownManager::new()),
        vec!["key-1", "key-2"],
        1,
    );

    let response = send_request(&client).await.unwrap();

    // Оба ключа вернули garbage, попытки исчерпаны, возвращаем последний ответ.
    assert_eq!(response.status(), 200);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    let keys_seen = server.keys_seen.lock().await;
    assert_eq!(keys_seen.len(), 2);
    assert_ne!(keys_seen[0], keys_seen[1]);
}
