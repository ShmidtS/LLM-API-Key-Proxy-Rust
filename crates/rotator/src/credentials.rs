use dashmap::DashMap;
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub struct CredentialEntry {
    pub key: String,
    pub provider: String,
    pub concurrent_limit: usize,
    pub current_requests: Arc<AtomicUsize>,
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
        let regex = Regex::new(r"^([A-Z_]+)_API_KEY(?:_(\d+))?$").unwrap();
        let mut keys_by_provider: HashMap<String, Vec<(usize, String)>> = HashMap::new();

        for (name, value) in std::env::vars() {
            if let Some(captures) = regex.captures(&name) {
                let provider = match &captures[1] {
                    "NVIDIA_NIM" => "nvidia".to_string(),
                    provider => provider.to_lowercase(),
                };
                let index = captures
                    .get(2)
                    .and_then(|capture| capture.as_str().parse().ok())
                    .unwrap_or(0);
                keys_by_provider
                    .entry(provider)
                    .or_default()
                    .push((index, value));
            }
        }

        for (provider, mut keys) in keys_by_provider {
            keys.sort_by_key(|(index, _)| *index);
            manager.register_keys(provider, keys.into_iter().map(|(_, key)| key).collect(), 10);
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
                current_requests: Arc::new(AtomicUsize::new(0)),
            })
            .collect();
        self.credentials.insert(provider, entries);
    }

    pub fn get_least_loaded(&self, provider: &str) -> Option<CredentialEntry> {
        self.credentials
            .get(provider)?
            .iter()
            .filter(|e| e.current_requests.load(Ordering::Relaxed) < e.concurrent_limit)
            .min_by_key(|e| e.current_requests.load(Ordering::Relaxed))
            .cloned()
    }

    pub fn acquire_least_loaded(&self, provider: &str) -> Option<CredentialEntry> {
        let entries = self.credentials.get(provider)?;
        loop {
            let entry = entries
                .iter()
                .filter_map(|e| {
                    let current = e.current_requests.load(Ordering::Acquire);
                    (current < e.concurrent_limit).then_some((current, e))
                })
                .min_by_key(|(current, _)| *current)?
                .1
                .clone();
            let current = entry.current_requests.load(Ordering::Acquire);
            if current >= entry.concurrent_limit {
                continue;
            }
            if entry
                .current_requests
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(entry);
            }
        }
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
    fn get_least_loaded_respects_shared_concurrency_counters() {
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string()],
            1,
        );

        let first = manager.acquire_least_loaded("openai").unwrap();
        let second = manager.acquire_least_loaded("openai").unwrap();

        assert_ne!(first.key, second.key);
    }

    #[test]
    fn get_least_loaded_skips_keys_at_concurrent_limit() {
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string()],
            1,
        );

        manager.increment("openai", "key-0");

        let selected = manager.get_least_loaded("openai").unwrap();

        assert_eq!(selected.key, "key-1");
    }

    #[test]
    fn from_env_groups_api_keys_by_provider() {
        let vars = [
            ("OPENAI_API_KEY", "openai-base-key"),
            ("OPENAI_API_KEY_0", "openai-key-0"),
            ("OPENAI_API_KEY_1", "openai-key-1"),
            ("GEMINI_CLI_API_KEY_1", "gemini-cli-key-1"),
            ("NVIDIA_NIM_API_KEY", "nvidia-base-key"),
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
        assert_eq!(
            openai_keys,
            vec!["openai-base-key", "openai-key-0", "openai-key-1"]
        );
        assert!(openai.iter().all(|entry| entry.concurrent_limit == 10));

        let gemini_cli = manager.credentials.get("gemini_cli").unwrap();
        let mut gemini_cli_keys: Vec<_> =
            gemini_cli.iter().map(|entry| entry.key.as_str()).collect();
        gemini_cli_keys.sort_unstable();
        assert_eq!(gemini_cli_keys, vec!["gemini-cli-key-1"]);
        assert!(gemini_cli.iter().all(|entry| entry.concurrent_limit == 10));

        let nvidia = manager.credentials.get("nvidia").unwrap();
        let nvidia_keys: Vec<_> = nvidia.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(nvidia_keys, vec!["nvidia-base-key"]);

        for (name, _) in vars {
            unsafe {
                remove_test_var(name);
            }
        }
    }
}
