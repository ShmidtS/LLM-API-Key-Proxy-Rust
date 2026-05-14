use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time;

const DEFAULT_USAGE_PATH: &str = "data/key_usage.json";
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_BATCH_SIZE: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageEntry {
    pub provider: String,
    pub key: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub timestamp: u64,
}

#[derive(Debug, Default)]
struct UsageState {
    usage: HashMap<(String, String), UsageEntry>,
    pending_events: usize,
    shutting_down: bool,
}

#[derive(Debug, Default)]
pub struct UsageManager {
    state: Arc<Mutex<UsageState>>,
    notify_flush: Arc<Notify>,
    task: Mutex<Option<JoinHandle<io::Result<()>>>>,
    batch_size: usize,
}

impl UsageManager {
    pub fn new() -> Self {
        Self::with_path(DEFAULT_USAGE_PATH)
    }

    pub fn with_path(path: impl AsRef<Path>) -> Self {
        Self::with_config(path, DEFAULT_FLUSH_INTERVAL, DEFAULT_BATCH_SIZE)
    }

    pub fn with_config(
        path: impl AsRef<Path>,
        flush_interval: Duration,
        batch_size: usize,
    ) -> Self {
        let path = path.as_ref().to_path_buf();
        let state = Arc::new(Mutex::new(UsageState::default()));
        let notify_flush = Arc::new(Notify::new());
        let task = tokio::spawn(flush_task(
            Arc::clone(&state),
            Arc::clone(&notify_flush),
            path,
            flush_interval,
        ));

        let manager = Self {
            state,
            notify_flush,
            task: Mutex::new(Some(task)),
            batch_size,
        };

        if batch_size == 0 {
            manager.notify_flush.notify_one();
        }

        manager
    }

    pub fn record_usage(&self, provider: &str, key: &str, prompt: u32, completion: u32) {
        let mut state = self.state.lock().expect("usage state mutex poisoned");
        let usage_key = (provider.to_owned(), key.to_owned());
        let now = current_timestamp();

        let entry = state.usage.entry(usage_key).or_insert_with(|| UsageEntry {
            provider: provider.to_owned(),
            key: key.to_owned(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            timestamp: now,
        });

        entry.prompt_tokens = entry.prompt_tokens.saturating_add(prompt);
        entry.completion_tokens = entry.completion_tokens.saturating_add(completion);
        entry.total_tokens = entry
            .total_tokens
            .saturating_add(prompt.saturating_add(completion));
        entry.timestamp = now;
        state.pending_events = state.pending_events.saturating_add(1);

        if state.pending_events >= self.batch_size {
            self.notify_flush.notify_one();
        }
    }

    pub fn get_usage(&self, provider: &str, key: &str) -> Option<UsageEntry> {
        self.state
            .lock()
            .expect("usage state mutex poisoned")
            .usage
            .get(&(provider.to_owned(), key.to_owned()))
            .cloned()
    }

    pub fn get_all_usage(&self) -> Vec<UsageEntry> {
        self.state
            .lock()
            .expect("usage state mutex poisoned")
            .usage
            .values()
            .cloned()
            .collect()
    }

    pub async fn shutdown(&self) -> io::Result<()> {
        {
            let mut state = self.state.lock().expect("usage state mutex poisoned");
            state.shutting_down = true;
        }
        self.notify_flush.notify_one();

        let task = self.task.lock().expect("usage task mutex poisoned").take();
        if let Some(task) = task {
            task.await.map_err(io::Error::other)??;
        }

        Ok(())
    }
}

async fn flush_task(
    state: Arc<Mutex<UsageState>>,
    notify_flush: Arc<Notify>,
    path: PathBuf,
    flush_interval: Duration,
) -> io::Result<()> {
    let mut interval = time::interval(flush_interval);

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = notify_flush.notified() => {}
        }

        flush_usage(&state, &path).await?;

        if state
            .lock()
            .expect("usage state mutex poisoned")
            .shutting_down
        {
            return Ok(());
        }
    }
}

async fn flush_usage(state: &Mutex<UsageState>, path: &Path) -> io::Result<()> {
    let (entries, flushed_events) = {
        let state = state.lock().expect("usage state mutex poisoned");
        if state.pending_events == 0 {
            return Ok(());
        }

        (
            state.usage.values().cloned().collect::<Vec<_>>(),
            state.pending_events,
        )
    };

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let json = serde_json::to_string_pretty(&entries).map_err(io::Error::other)?;
    tokio::fs::write(path, json).await?;

    let mut state = state.lock().expect("usage state mutex poisoned");
    state.pending_events = state.pending_events.saturating_sub(flushed_events);
    Ok(())
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
