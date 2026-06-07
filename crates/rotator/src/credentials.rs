use crate::error::{Result, RotatorError};
use dashmap::DashMap;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
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
    pub credentials: Arc<DashMap<String, Vec<CredentialEntry>>>,
    key_index: Arc<DashMap<String, DashMap<String, usize>>>,
}

#[derive(Debug)]
pub struct CredentialPermit {
    manager: Arc<CredentialManager>,
    provider: String,
    key: String,
}

impl CredentialPermit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manager: Arc<CredentialManager>,
        provider: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            provider: provider.into(),
            key: key.into(),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Drop for CredentialPermit {
    fn drop(&mut self) {
        self.manager.decrement(&self.provider, &self.key);
    }
}

impl CredentialManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_env() -> Self {
        // Load .env by walking up from cwd into std::env so credentials are
        // found even when the process was started from a different directory.
        let _ = dotenvy::dotenv_override();
        let dotenv_path = proxy_config::find_env_file();
        Self::from_env_and_dotenv(dotenv_path.as_deref())
    }

    pub fn from_env_and_dotenv(dotenv_path: Option<&Path>) -> Self {
        let manager = Self::new();
        let regex = Regex::new(r"^([A-Z_]+)_API_KEY(?:_(\d+))?$").unwrap();
        let mut unique_keys: std::collections::BTreeMap<String, (String, usize, String)> =
            std::collections::BTreeMap::new();

        // 1. Load from std::env
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
                unique_keys.insert(name, (provider, index, value));
            }
        }

        // 2. Fallback: read .env file directly so keys are found even when the
        //    process was started from a different working directory.
        if let Some(path) = dotenv_path
            && let Ok(contents) = std::fs::read_to_string(path)
        {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((key_name, value)) = line.split_once('=') else {
                    continue;
                };
                let key_name = key_name.trim();
                if !key_name.contains("_API_KEY") {
                    continue;
                }
                if key_name == "PROXY_API_KEY" {
                    continue;
                }
                let mut value = value.trim();
                value = value.strip_prefix('"').unwrap_or(value);
                value = value.strip_suffix('"').unwrap_or(value);
                value = value.strip_prefix('\'').unwrap_or(value);
                value = value.strip_suffix('\'').unwrap_or(value);
                if value.is_empty() || value.starts_with("YOUR_") {
                    continue;
                }
                if let Some(captures) = regex.captures(key_name) {
                    let provider = match &captures[1] {
                        "NVIDIA_NIM" => "nvidia".to_string(),
                        provider => provider.to_lowercase(),
                    };
                    let index = captures
                        .get(2)
                        .and_then(|capture| capture.as_str().parse().ok())
                        .unwrap_or(0);
                    unique_keys.insert(key_name.to_string(), (provider, index, value.to_string()));
                }
            }
        }

        let mut keys_by_provider: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        for (_, (provider, index, value)) in unique_keys {
            keys_by_provider
                .entry(provider)
                .or_default()
                .push((index, value));
        }

        let mut loaded_counts: Vec<(String, usize)> = Vec::new();
        for (provider, mut keys) in keys_by_provider {
            let count = keys.len();
            keys.sort_by_key(|(index, _)| *index);
            manager.register_keys(
                provider.clone(),
                keys.into_iter().map(|(_, key)| key).collect(),
                50,
            );
            loaded_counts.push((provider, count));
        }

        if !loaded_counts.is_empty() {
            let summary = loaded_counts
                .iter()
                .map(|(p, c)| format!("{}={}", p, c))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::info!("credentials loaded: {}", summary);
        } else {
            tracing::info!("credentials loaded: none");
        }

        let _ = manager.discover_oauth_credentials();
        manager
    }

    pub fn discover_oauth_credentials(&self) -> Result<()> {
        self.discover_oauth_credentials_in("oauth_creds")
    }

    pub fn discover_oauth_credentials_in(&self, dir: impl AsRef<Path>) -> Result<()> {
        let dir = dir.as_ref();
        if !dir.exists() {
            return Ok(());
        }
        let regex = Regex::new(r"^([a-z0-9_]+)_oauth_(\d+)\.json$").unwrap();
        let mut keys_by_provider: HashMap<String, Vec<(usize, String)>> = HashMap::new();

        for entry in std::fs::read_dir(dir)
            .map_err(|error| RotatorError::Other(format!("failed to read oauth_creds: {error}")))?
        {
            let entry = entry.map_err(|error| {
                RotatorError::Other(format!("failed to read oauth_creds entry: {error}"))
            })?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            let Some(captures) = regex.captures(&file_name) else {
                continue;
            };
            let provider = match &captures[1] {
                "google" => "gemini".to_owned(),
                provider => provider.to_owned(),
            };
            let index = captures[2].parse().unwrap_or(0);
            let body = std::fs::read_to_string(entry.path()).map_err(|error| {
                RotatorError::Other(format!("failed to read OAuth credential file: {error}"))
            })?;
            serde_json::from_str::<crate::providers::oauth::OAuthCredentialFile>(&body)?;
            keys_by_provider
                .entry(provider)
                .or_default()
                .push((index, body));
        }

        for (provider, mut keys) in keys_by_provider {
            keys.sort_by_key(|(index, _)| *index);
            self.register_keys(provider, keys.into_iter().map(|(_, key)| key).collect(), 50);
        }
        Ok(())
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
        let index_map: DashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.key.clone(), i))
            .collect();
        self.credentials.insert(provider.clone(), entries);
        self.key_index.insert(provider, index_map);
    }

    pub fn get_least_loaded(&self, provider: &str) -> Option<CredentialEntry> {
        self.credentials
            .get(provider)?
            .iter()
            .filter(|e| e.current_requests.load(Ordering::Relaxed) < e.concurrent_limit)
            .min_by_key(|e| e.current_requests.load(Ordering::Relaxed))
            .cloned()
    }

    pub fn get_key_status(&self, provider: &str) -> Vec<(String, usize, usize)> {
        self.credentials
            .get(provider)
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| {
                        (
                            e.key.clone(),
                            e.current_requests.load(Ordering::Relaxed),
                            e.concurrent_limit,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn has_any_keys(&self, provider: &str) -> bool {
        self.credentials
            .get(provider)
            .map(|e| !e.is_empty())
            .unwrap_or(false)
    }

    pub fn acquire_least_loaded(&self, provider: &str) -> Option<CredentialEntry> {
        self.acquire_least_loaded_where(provider, |_| true)
    }

    pub(crate) fn acquire_least_loaded_where<F>(
        &self,
        provider: &str,
        is_available: F,
    ) -> Option<CredentialEntry>
    where
        F: Fn(&str) -> bool,
    {
        let entries = self.credentials.get(provider)?;
        loop {
            let entry = entries
                .iter()
                .filter_map(|e| {
                    let current = e.current_requests.load(Ordering::Acquire);
                    (current < e.concurrent_limit && is_available(&e.key)).then_some((current, e))
                })
                .min_by_key(|(current, _)| *current)?
                .1
                .clone();
            let current = entry.current_requests.load(Ordering::Acquire);
            if current >= entry.concurrent_limit || !is_available(&entry.key) {
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
        if let Some(index_map) = self.key_index.get(provider)
            && let Some(index) = index_map.get(key)
            && let Some(entries) = self.credentials.get(provider)
            && let Some(entry) = entries.get(*index)
        {
            entry.current_requests.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Fallback to linear scan
        if let Some(entries) = self.credentials.get(provider) {
            for entry in entries.iter() {
                if entry.key == key {
                    entry.current_requests.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn decrement(&self, provider: &str, key: &str) {
        if let Some(index_map) = self.key_index.get(provider)
            && let Some(index) = index_map.get(key)
            && let Some(entries) = self.credentials.get(provider)
            && let Some(entry) = entries.get(*index)
        {
            let _ = entry.current_requests.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        // Fallback to linear scan
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

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn discovers_oauth_credential_files() {
        let dir = std::env::temp_dir().join(format!(
            "rotator-oauth-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("google_oauth_1.json");
        std::fs::write(
            &file,
            r#"{"access_token":"access","refresh_token":"refresh","expires_at":123,"token_type":"Bearer","client_id":"client","token_endpoint":"https://oauth2.googleapis.com/token"}"#,
        )
        .unwrap();

        let manager = CredentialManager::new();
        manager.discover_oauth_credentials_in(&dir).unwrap();

        let entries = manager.credentials.get("gemini").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].key.contains("\"access_token\":\"access\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn from_env_groups_api_keys_by_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
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

        let manager = CredentialManager::from_env_and_dotenv(None);

        let openai = manager.credentials.get("openai").unwrap();
        let mut openai_keys: Vec<_> = openai.iter().map(|entry| entry.key.as_str()).collect();
        openai_keys.sort_unstable();
        assert_eq!(
            openai_keys,
            vec!["openai-base-key", "openai-key-0", "openai-key-1"]
        );
        assert!(openai.iter().all(|entry| entry.concurrent_limit == 50));

        let gemini_cli = manager.credentials.get("gemini_cli").unwrap();
        let mut gemini_cli_keys: Vec<_> =
            gemini_cli.iter().map(|entry| entry.key.as_str()).collect();
        gemini_cli_keys.sort_unstable();
        assert_eq!(gemini_cli_keys, vec!["gemini-cli-key-1"]);
        assert!(gemini_cli.iter().all(|entry| entry.concurrent_limit == 50));

        let nvidia = manager.credentials.get("nvidia").unwrap();
        let nvidia_keys: Vec<_> = nvidia.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(nvidia_keys, vec!["nvidia-base-key"]);

        for (name, _) in vars {
            unsafe {
                remove_test_var(name);
            }
        }
    }

    #[test]
    fn from_env_fallback_reads_dotenv_file_directly() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "rotator-dotenv-fallback-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join(".env");
        std::fs::write(
            &env_file,
            "ANTHROPIC_API_KEY_1=anthropic-val-from-dotenv\n\
             OPENAI_API_KEY=openai-val\n\
             PROXY_API_KEY=should-be-ignored\n\
             # comment\n\
             EMPTY_API_KEY=\n\
             YOUR_API_KEY=YOUR_KEY_HERE\n\
             UNKNOWN_VAR=hello\n",
        )
        .unwrap();

        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY_1");
            std::env::remove_var("OPENAI_API_KEY");
        }

        let manager = CredentialManager::from_env_and_dotenv(Some(&env_file));

        let anthropic = manager.credentials.get("anthropic").unwrap();
        assert_eq!(anthropic.len(), 1);
        assert_eq!(anthropic[0].key, "anthropic-val-from-dotenv");

        let openai = manager.credentials.get("openai").unwrap();
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0].key, "openai-val");

        assert!(manager.credentials.get("proxy").is_none());
        assert!(manager.credentials.get("empty").is_none());
        assert!(manager.credentials.get("your").is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auth_token_does_not_match_api_key_regex() {
        let regex = Regex::new(r"^([A-Z_]+)_API_KEY(?:_(\d+))?$").unwrap();
        assert!(!regex.is_match("ANTHROPIC_AUTH_TOKEN"));
        assert!(!regex.is_match("ANTHROPIC_BASE_URL"));
    }

    #[test]
    fn from_env_reads_dotenv_from_arbitrary_cwd() {
        let _guard = ENV_LOCK.lock().unwrap();

        let dir = std::env::temp_dir().join(format!(
            "rotator-cwd-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join(".env");
        std::fs::write(&env_file, "ARBITRARYPROV_API_KEY_1=arb-val-from-cwd\n").unwrap();

        unsafe {
            std::env::remove_var("ARBITRARYPROV_API_KEY");
            std::env::remove_var("ARBITRARYPROV_API_KEY_1");
        }

        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let manager = CredentialManager::from_env();

        std::env::set_current_dir(original_cwd).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let arb = manager.credentials.get("arbitraryprov").unwrap();
        assert_eq!(arb.len(), 1);
        assert_eq!(arb[0].key, "arb-val-from-cwd");

        unsafe {
            std::env::remove_var("ARBITRARYPROV_API_KEY_1");
        }
    }
}
