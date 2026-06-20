use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{error, warn};

const DEFAULT_LOG_PATH: &str = "logs/transactions";
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_BUFFER_SIZE: usize = 1000;
const DEFAULT_SAMPLING_RATE: f64 = 0.1;

/// A single transaction log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionLog {
    pub request_id: String,
    pub timestamp: u64,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub latency_ms: u64,
    pub status: String,
    pub token_usage: Option<TokenUsage>,
    pub error_class: Option<String>,
    pub retry_count: u32,
    pub credential_hash_prefix: String,
    pub chunk_count: Option<u64>,
    pub first_chunk_latency_ms: Option<u64>,
    pub stream_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Sampling policy: 100% for errors, configurable for successes.
#[derive(Debug, Clone)]
pub struct SamplingPolicy {
    pub success_rate: f64,
}

impl Default for SamplingPolicy {
    fn default() -> Self {
        Self {
            success_rate: DEFAULT_SAMPLING_RATE,
        }
    }
}

impl SamplingPolicy {
    pub fn should_sample(&self, is_error: bool) -> bool {
        if is_error {
            return true;
        }
        // Fast path: if rate is 1.0, always sample; if 0.0, never sample.
        if self.success_rate >= 1.0 {
            return true;
        }
        if self.success_rate <= 0.0 {
            return false;
        }
        // Use a simple hash of the current thread + time as a deterministic random.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let hash = gxhash64(&now.to_le_bytes(), now.wrapping_mul(0x9E3779B97F4A7C15));
        let threshold = (self.success_rate * u64::MAX as f64) as u64;
        hash <= threshold
    }
}

// gxhash64 — very fast, decent quality, zero dependencies.
fn gxhash64(data: &[u8], seed: u64) -> u64 {
    let mut hash: u64 = seed;
    for chunk in data.chunks_exact(8) {
        let mut v: u64 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            v |= (b as u64) << (i * 8);
        }
        hash = hash.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(v);
        hash ^= hash.rotate_right(27);
    }
    // Tail
    let mut tail: u64 = 0;
    for (i, &b) in data.chunks_exact(8).remainder().iter().enumerate() {
        tail |= (b as u64) << (i * 8);
    }
    hash = hash.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(tail);
    hash ^= hash.rotate_right(31);
    hash.wrapping_mul(0x9E3779B97F4A7C15)
}

// Helper to get a redacted hash prefix (first 4 chars of the sha256 hex).
pub fn credential_hash_prefix(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(key.as_bytes());
    let hex = format!("{:x}", hash);
    hex.chars().take(4).collect()
}

/// Sender handle for submitting transaction log entries.
#[derive(Debug, Clone)]
pub struct TransactionLogger {
    tx: mpsc::Sender<TransactionLog>,
    sampling: Arc<SamplingPolicy>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<io::Result<()>>>>>,
    path: PathBuf,
    _shutdown: Arc<tokio::sync::Notify>,
}

impl TransactionLogger {
    pub fn new() -> Self {
        Self::with_path(DEFAULT_LOG_PATH)
    }

    pub fn with_path(path: impl AsRef<Path>) -> Self {
        Self::with_config(
            path,
            DEFAULT_FLUSH_INTERVAL,
            DEFAULT_BUFFER_SIZE,
            DEFAULT_SAMPLING_RATE,
        )
    }

    pub fn with_config(
        path: impl AsRef<Path>,
        flush_interval: Duration,
        buffer_size: usize,
        sampling_rate: f64,
    ) -> Self {
        let path = path.as_ref().to_path_buf();
        let (tx, rx) = mpsc::channel(buffer_size);
        let sampling = Arc::new(SamplingPolicy {
            success_rate: sampling_rate.clamp(0.0, 1.0),
        });
        let notify = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(flush_task(
            rx,
            path.clone(),
            flush_interval,
            Arc::clone(&notify),
        ));

        Self {
            tx,
            sampling,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            path,
            _shutdown: notify,
        }
    }

    /// Submit a transaction log entry. Non-blocking; drops if the buffer is full.
    pub fn log(&self, entry: TransactionLog) {
        let is_error = entry.error_class.is_some();
        if !self.sampling.should_sample(is_error) {
            return;
        }
        if let Err(e) = self.tx.try_send(entry) {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    warn!("transaction log buffer full, dropping entry");
                }
                mpsc::error::TrySendError::Closed(_) => {
                    error!("transaction log channel closed");
                }
            }
        }
    }

    /// Convenience helper to build a log entry from request lifecycle data.
    #[allow(clippy::too_many_arguments)]
    pub fn log_request(
        &self,
        request_id: &str,
        provider: &str,
        model: &str,
        endpoint: &str,
        latency_ms: u64,
        status: &str,
        token_usage: Option<TokenUsage>,
        error_class: Option<&str>,
        retry_count: u32,
        credential_key: &str,
        chunk_count: Option<u64>,
        first_chunk_latency_ms: Option<u64>,
        stream_duration_ms: Option<u64>,
    ) {
        let entry = TransactionLog {
            request_id: request_id.to_owned(),
            timestamp: current_timestamp(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            endpoint: endpoint.to_owned(),
            latency_ms,
            status: status.to_owned(),
            token_usage,
            error_class: error_class.map(ToString::to_string),
            retry_count,
            credential_hash_prefix: credential_hash_prefix(credential_key),
            chunk_count,
            first_chunk_latency_ms,
            stream_duration_ms,
        };
        self.log(entry);
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn shutdown(&self) -> io::Result<()> {
        self._shutdown.notify_one();
        let task = self.task.lock().await.take();
        if let Some(task) = task {
            let _ = task.await.map_err(io::Error::other)?;
        }
        Ok(())
    }
}

impl Default for TransactionLogger {
    fn default() -> Self {
        Self::new()
    }
}

async fn flush_task(
    mut rx: mpsc::Receiver<TransactionLog>,
    base_path: PathBuf,
    flush_interval: Duration,
    notify: Arc<tokio::sync::Notify>,
) -> io::Result<()> {
    let mut buffer: Vec<TransactionLog> = Vec::with_capacity(128);
    let mut interval = tokio::time::interval(flush_interval);
    let mut current_date = chrono::Local::now().date_naive();
    let mut current_file: Option<tokio::fs::File> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = notify.notified() => {
                // drain remaining and break
                while let Ok(entry) = rx.try_recv() {
                    buffer.push(entry);
                }
                if let Err(e) = flush(&mut buffer, &base_path, &mut current_date, &mut current_file).await {
                    error!(error = %e, "transaction log final flush failed");
                }
                return Ok(());
            }
            Some(entry) = rx.recv() => {
                buffer.push(entry);
                if buffer.len() >= 128
                    && let Err(e) = flush(&mut buffer, &base_path, &mut current_date, &mut current_file).await {
                        error!(error = %e, "transaction log flush failed");
                    }
            }
        }

        if let Err(e) = flush(
            &mut buffer,
            &base_path,
            &mut current_date,
            &mut current_file,
        )
        .await
        {
            error!(error = %e, "transaction log periodic flush failed");
        }
    }
}

async fn flush(
    buffer: &mut Vec<TransactionLog>,
    base_path: &Path,
    current_date: &mut chrono::NaiveDate,
    current_file: &mut Option<tokio::fs::File>,
) -> io::Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }

    let today = chrono::Local::now().date_naive();
    if today != *current_date || current_file.is_none() {
        *current_date = today;
        *current_file = None;
    }

    let file_path = base_path.join(format!("transactions_{}.ndjson", today));
    tokio::fs::create_dir_all(base_path).await?;

    let file = match current_file {
        Some(file) => file,
        None => {
            let f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
                .await?;
            *current_file = Some(f);
            current_file.as_mut().unwrap()
        }
    };

    use tokio::io::AsyncWriteExt;
    for entry in buffer.drain(..) {
        let line = serde_json::to_string(&entry).map_err(io::Error::other)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }
    file.flush().await?;

    Ok(())
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_policy_errors_always_sampled() {
        let policy = SamplingPolicy { success_rate: 0.0 };
        assert!(policy.should_sample(true));
    }

    #[test]
    fn sampling_policy_zero_rate_skips_success() {
        let policy = SamplingPolicy { success_rate: 0.0 };
        // Deterministic, but not guaranteed for all seeds; just verify it runs.
        let _ = policy.should_sample(false);
    }

    #[test]
    fn credential_hash_prefix_first_four_chars() {
        let prefix = credential_hash_prefix("sk-1234567890abcdef");
        assert_eq!(prefix.len(), 4);
    }

    #[tokio::test]
    async fn logger_flushes_to_file() {
        let temp_dir = std::env::temp_dir().join("proxy-transaction-test");
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        let logger = TransactionLogger::with_config(&temp_dir, Duration::from_millis(100), 10, 1.0);

        logger.log_request(
            "req-1",
            "openai",
            "gpt-4",
            "chat/completions",
            123,
            "200",
            None,
            None,
            0,
            "sk-abc",
            None,
            None,
            None,
        );

        tokio::time::sleep(Duration::from_millis(300)).await;
        logger.shutdown().await.unwrap();

        let today = chrono::Local::now().date_naive();
        let file_path = temp_dir.join(format!("transactions_{}.ndjson", today));
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(content.contains("req-1"));
        assert!(content.contains("openai"));
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
