use dashmap::DashMap;
use regex::Regex;
use std::collections::HashMap;
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

    pub fn from_env() -> Self {
        let manager = Self::new();
        let regex = Regex::new(r"^([A-Z_]+)_API_KEY_(\d+)$").unwrap();
        let mut keys_by_provider: HashMap<String, Vec<String>> = HashMap::new();

        for (name, value) in std::env::vars() {
            if let Some(captures) = regex.captures(&name) {
                let provider = captures[1].to_lowercase();
                keys_by_provider.entry(provider).or_default().push(value);
            }
        }

        for (provider, keys) in keys_by_provider {
            manager.register_keys(provider, keys, 10);
        }

        manager
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

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn set_test_var(name: &str, value: &str) {
        unsafe {
            std::env::set_var(name, value);
        }
    }

    unsafe fn remove_test_var(name: &str) {
        unsafe {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn from_env_groups_api_keys_by_provider() {
        let vars = [
            ("OPENAI_API_KEY_0", "openai-key-0"),
            ("OPENAI_API_KEY_1", "openai-key-1"),
            ("GEMINI_CLI_API_KEY_1", "gemini-cli-key-1"),
        ];

        for (name, _) in vars {
            unsafe {
                remove_test_var(name);
            }
        }

        for (name, value) in vars {
            unsafe {
                set_test_var(name, value);
            }
        }

        let manager = CredentialManager::from_env();

        let openai = manager.credentials.get("openai").unwrap();
        let mut openai_keys: Vec<_> = openai.iter().map(|entry| entry.key.as_str()).collect();
        openai_keys.sort_unstable();
        assert_eq!(openai_keys, vec!["openai-key-0", "openai-key-1"]);
        assert!(openai.iter().all(|entry| entry.concurrent_limit == 10));

        let gemini_cli = manager.credentials.get("gemini_cli").unwrap();
        let mut gemini_cli_keys: Vec<_> =
            gemini_cli.iter().map(|entry| entry.key.as_str()).collect();
        gemini_cli_keys.sort_unstable();
        assert_eq!(gemini_cli_keys, vec!["gemini-cli-key-1"]);
        assert!(gemini_cli.iter().all(|entry| entry.concurrent_limit == 10));

        for (name, _) in vars {
            unsafe {
                remove_test_var(name);
            }
        }
    }
}
