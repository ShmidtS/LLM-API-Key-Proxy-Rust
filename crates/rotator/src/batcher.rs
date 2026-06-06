use crate::error::{Result, RotatorError};
use crate::{ProviderRegistry, RotatorClient};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, timeout};

const EMBEDDING_BATCH_SIZE: usize = 64;
const EMBEDDING_BATCH_TIMEOUT: Duration = Duration::from_millis(100);

type BatchKey = (
    String,
    Option<String>,
    Option<u64>,
    Option<String>,
    Option<String>,
);

#[derive(Debug, Clone)]
pub struct EmbeddingBatcher {
    sender: mpsc::Sender<EmbeddingJob>,
}

#[derive(Debug)]
struct EmbeddingJob {
    body: Value,
    tx: oneshot::Sender<Result<reqwest::Response>>,
}

impl EmbeddingBatcher {
    pub fn new(rotator: Arc<RotatorClient>, registry: Arc<ProviderRegistry>) -> Self {
        let (sender, receiver) = mpsc::channel(1024);
        tokio::spawn(worker(rotator, registry, receiver));
        Self { sender }
    }

    pub async fn add_request(&self, body: Value) -> Result<reqwest::Response> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(EmbeddingJob { body, tx })
            .await
            .map_err(|_| RotatorError::Other("embedding batcher is shut down".to_owned()))?;
        rx.await
            .map_err(|_| RotatorError::Other("embedding batcher worker stopped".to_owned()))?
    }
}

async fn worker(
    rotator: Arc<RotatorClient>,
    registry: Arc<ProviderRegistry>,
    mut receiver: mpsc::Receiver<EmbeddingJob>,
) {
    while let Some(first) = receiver.recv().await {
        let mut batch = vec![first];
        let deadline = Instant::now() + EMBEDDING_BATCH_TIMEOUT;

        while batch.len() < EMBEDDING_BATCH_SIZE {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match timeout(remaining, receiver.recv()).await {
                Ok(Some(job)) => batch.push(job),
                Ok(None) | Err(_) => break,
            }
        }

        process_batch(&rotator, &registry, batch).await;
    }
}

async fn process_batch(
    rotator: &RotatorClient,
    registry: &ProviderRegistry,
    batch: Vec<EmbeddingJob>,
) {
    let mut grouped: HashMap<BatchKey, Vec<EmbeddingJob>> = HashMap::new();

    for job in batch {
        if is_list_input(&job.body) {
            process_group(rotator, registry, vec![job]).await;
            continue;
        }

        match batch_key(&job.body) {
            Ok(key) => grouped.entry(key).or_default().push(job),
            Err(error) => {
                let _ = job.tx.send(Err(error));
            }
        }
    }

    for group in grouped.into_values() {
        process_group(rotator, registry, group).await;
    }
}

async fn process_group(
    rotator: &RotatorClient,
    registry: &ProviderRegistry,
    group: Vec<EmbeddingJob>,
) {
    let Ok(key) = batch_key(&group[0].body) else {
        return;
    };
    let provider = registry
        .resolve_provider_by_model(&key.0)
        .or_else(|| registry.find_provider_for_model(&key.0))
        .unwrap_or_else(|| "openai".to_owned());
    let list_input = is_list_input(&group[0].body);

    let merged_body = match merged_body(&group, list_input) {
        Ok(body) => body,
        Err(error) => {
            send_error(group, error);
            return;
        }
    };

    match rotator.request(&provider, "embeddings", merged_body).await {
        Ok(resp) if group.len() == 1 => {
            if let Some(job) = group.into_iter().next() {
                let _ = job.tx.send(Ok(resp));
            }
        }
        Ok(resp) => match resp.json::<Value>().await {
            Ok(response) => split_response(group, response),
            Err(error) => send_error(group, RotatorError::Http(error.to_string())),
        },
        Err(error) => send_error(group, error),
    }
}

fn batch_key(body: &Value) -> Result<BatchKey> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| RotatorError::Other("embedding request missing model".to_owned()))?
        .to_owned();
    let input_type = body
        .get("input_type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let dimensions = body.get("dimensions").and_then(Value::as_u64);
    let user = body
        .get("user")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let encoding_format = body
        .get("encoding_format")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Ok((model, input_type, dimensions, user, encoding_format))
}

fn is_list_input(body: &Value) -> bool {
    body.get("input").and_then(Value::as_array).is_some()
}

fn merged_body(group: &[EmbeddingJob], list_input: bool) -> Result<Value> {
    let mut body = group[0].body.clone();
    if list_input {
        if group.len() != 1 {
            return Err(RotatorError::Other(
                "embedding batch requests with list input cannot be merged".to_owned(),
            ));
        }
        return Ok(body);
    }

    let inputs: Result<Vec<Value>> = group
        .iter()
        .map(|job| {
            job.body
                .get("input")
                .filter(|input| input.is_string())
                .cloned()
                .ok_or_else(|| {
                    RotatorError::Other("embedding request input must be a string".to_owned())
                })
        })
        .collect();
    body["input"] = Value::Array(inputs?);
    Ok(body)
}

fn split_response(group: Vec<EmbeddingJob>, response: Value) {
    let Some(data) = response.get("data").and_then(Value::as_array) else {
        send_error(
            group,
            RotatorError::Other("embedding response missing data array".to_owned()),
        );
        return;
    };

    let group_len = group.len();
    if data.len() < group_len {
        send_error(
            group,
            RotatorError::Other(format!(
                "batch response has {} items but batch has {} requests",
                data.len(),
                group_len
            )),
        );
        return;
    }

    for (index, job) in group.into_iter().enumerate() {
        let single = serde_json::json!({
            "object": response.get("object").cloned().unwrap_or(Value::Null),
            "model": response.get("model").cloned().unwrap_or(Value::Null),
            "data": [data[index].clone()],
            "usage": Value::Null,
        });
        let _ = job.tx.send(response_from_json(single));
    }
}

fn response_from_json(value: Value) -> Result<reqwest::Response> {
    let body = serde_json::to_vec(&value)?;
    Ok(http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(bytes::Bytes::from(body))
        .map_err(|error| RotatorError::Other(error.to_string()))?
        .into())
}

fn send_error(group: Vec<EmbeddingJob>, error: RotatorError) {
    for job in group {
        let _ = job.tx.send(Err(error.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool,
        ProviderDefinition, RateLimiterRegistry,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn embedding_server(expected_requests: usize) -> (String, Arc<Mutex<Vec<Value>>>) {
        let (base_url, requests, _) = embedding_server_with_paths(expected_requests).await;
        (base_url, requests)
    }

    async fn embedding_server_with_paths(
        expected_requests: usize,
    ) -> (String, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let server_paths = paths.clone();

        tokio::spawn(async move {
            for _ in 0..expected_requests {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0; 4096];
                let Ok(n) = socket.read(&mut buffer).await else {
                    return;
                };
                let text = String::from_utf8_lossy(&buffer[..n]);
                if let Some(path) = text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                {
                    server_paths.lock().unwrap().push(path.to_owned());
                }
                let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
                let request: Value = serde_json::from_str(body).unwrap();
                server_requests.lock().unwrap().push(request.clone());

                let input_count = request
                    .get("input")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(1);
                let data: Vec<_> = (0..input_count)
                    .map(|index| {
                        json!({
                            "object": "embedding",
                            "embedding": [index as f64],
                            "index": index,
                        })
                    })
                    .collect();
                let response_body = json!({
                    "object": "list",
                    "model": request["model"].clone(),
                    "data": data,
                    "usage": {"prompt_tokens": input_count, "total_tokens": input_count}
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        (format!("http://{addr}/v1"), requests, paths)
    }

    fn batcher(base_url: String) -> EmbeddingBatcher {
        provider_batcher(
            "openai",
            base_url,
            AuthType::Bearer,
            "^text-embedding-.*",
            vec!["/embeddings".to_owned()],
        )
    }

    fn provider_batcher(
        provider: &str,
        base_url: String,
        auth_type: AuthType,
        model_pattern: &str,
        endpoints: Vec<String>,
    ) -> EmbeddingBatcher {
        let registry = Arc::new(ProviderRegistry::default());
        registry.register(ProviderDefinition {
            id: provider.to_owned(),
            display_name: provider.to_owned(),
            base_url,
            auth_type,
            model_patterns: vec![model_pattern.to_owned()],
            compiled_patterns: vec![],
            endpoints,
            features: vec!["embeddings".to_owned()],
            model_count: 1,
            timeout_secs: 30,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });
        let credentials = CredentialManager::new();
        credentials.register_keys(provider.to_owned(), vec!["test-key".to_owned()], 1);
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

        EmbeddingBatcher::new(Arc::new(rotator), registry)
    }

    #[tokio::test]
    async fn merges_string_inputs_and_splits_response_data() {
        let (base_url, requests) = embedding_server(1).await;
        let batcher = batcher(base_url);

        let first = batcher.add_request(json!({
            "model": "text-embedding-3-small",
            "input": "one",
            "dimensions": 128,
            "user": "user-1"
        }));
        let second = batcher.add_request(json!({
            "model": "text-embedding-3-small",
            "input": "two",
            "dimensions": 128,
            "user": "user-1"
        }));
        let (first, second) = tokio::join!(first, second);

        let first = first.unwrap().json::<Value>().await.unwrap();
        let second = second.unwrap().json::<Value>().await.unwrap();
        assert_eq!(first["data"][0]["index"], 0);
        assert_eq!(second["data"][0]["index"], 1);

        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["input"], json!(["one", "two"]));
        assert_eq!(captured[0]["dimensions"], 128);
        assert_eq!(captured[0]["user"], "user-1");
    }

    #[tokio::test]
    async fn gemini_embeddings_use_native_model_endpoint() {
        let (base_url, _, paths) = embedding_server_with_paths(1).await;
        let batcher = provider_batcher(
            "gemini",
            base_url,
            AuthType::ApiKey,
            "^(models/)?gemini[-/].*",
            vec!["/embeddings".to_owned()],
        );

        let response = batcher
            .add_request(json!({
                "model": "gemini-embedding-001",
                "input": "hello"
            }))
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            paths.lock().unwrap()[0],
            "/v1/models/gemini-embedding-001:embedContent?key=test-key"
        );
    }

    #[tokio::test]
    async fn sends_list_inputs_without_merging() {
        let (base_url, requests) = embedding_server(2).await;
        let batcher = batcher(base_url);

        let first = batcher.add_request(json!({
            "model": "text-embedding-3-small",
            "input": ["one", "two"]
        }));
        let second = batcher.add_request(json!({
            "model": "text-embedding-3-small",
            "input": ["three", "four"]
        }));
        let (first, second) = tokio::join!(first, second);

        let first = first.unwrap().json::<Value>().await.unwrap();
        let second = second.unwrap().json::<Value>().await.unwrap();
        assert_eq!(first["data"].as_array().unwrap().len(), 2);
        assert_eq!(second["data"].as_array().unwrap().len(), 2);

        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["input"], json!(["one", "two"]));
        assert_eq!(captured[1]["input"], json!(["three", "four"]));
    }
}
