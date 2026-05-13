use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
pub struct CredentialEntry {
    pub key: String,
    pub provider: String,
    pub concurrent_limit: usize,
    pub current_requests: AtomicUsize,
}

impl Clone for CredentialEntry {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            provider: self.provider.clone(),
            concurrent_limit: self.concurrent_limit,
            current_requests: AtomicUsize::new(self.current_requests.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CredentialManager {
    credentials: Arc<DashMap<String, Vec<CredentialEntry>>>,
}

impl CredentialManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_keys(&self, provider: String, keys: Vec<String>, limit: usize) {
        let entries: Vec<_> = keys
            .into_iter()
            .map(|key| CredentialEntry {
                key,
                provider: provider.clone(),
                concurrent_limit: limit,
                current_requests: AtomicUsize::new(0),
            })
            .collect();
        self.credentials.insert(provider, entries);
    }

    pub fn get_least_loaded(&self, provider: &str) -> Option<CredentialEntry> {
        self.credentials
            .get(provider)?
            .iter()
            .min_by_key(|e| e.current_requests.load(Ordering::Relaxed))
            .cloned()
    }

    pub fn increment(&self, provider: &str, key: &str) {
        if let Some(entries) = self.credentials.get(provider) {
            for entry in entries.iter() {
                if entry.key == key {
                    entry.current_requests.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn decrement(&self, provider: &str, key: &str) {
        if let Some(entries) = self.credentials.get(provider) {
            for entry in entries.iter() {
                if entry.key == key {
                    let _ = entry.current_requests.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }
}
