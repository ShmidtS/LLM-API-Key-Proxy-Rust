use crate::error::{Result, RotatorError};
use dashmap::DashMap;
use rand::Rng;
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

/// Strategy for choosing a credential among several eligible keys for a
/// provider. Mirrors bifrost's `core/keyselectors`: least-loaded balances by
/// in-flight concurrency (the long-standing default), round-robin spreads
/// requests evenly in registration order, and weighted-random picks uniformly
/// at random (the zero-weight fallback of bifrost's `WeightedRandom`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionStrategy {
    #[default]
    LeastLoaded,
    RoundRobin,
    WeightedRandom,
}

impl SelectionStrategy {
    /// Parse a human-friendly strategy name (case-insensitive, accepts `-`/`_`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "least-loaded" | "least_loaded" | "leastloaded" | "least" => Some(Self::LeastLoaded),
            "round-robin" | "round_robin" | "roundrobin" | "round" => Some(Self::RoundRobin),
            "weighted-random" | "weighted_random" | "weightedrandom" | "weighted" | "random" => {
                Some(Self::WeightedRandom)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CredentialManager {
    pub credentials: Arc<DashMap<String, Vec<CredentialEntry>>>,
    key_index: Arc<DashMap<String, DashMap<String, usize>>>,
    /// Per-provider selection strategy. The empty-string key holds the
    /// configured default applied to any provider without an explicit entry.
    strategies: Arc<DashMap<String, SelectionStrategy>>,
    /// Round-robin cursor per provider (monotonic; index derived via modulo).
    rr_counters: Arc<DashMap<String, AtomicUsize>>,
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

    /// Number of credentials registered for `provider` (regardless of cooldown),
    /// in O(1) and without materializing the status `Vec` that `get_key_status`
    /// allocates. Used to bound request-level (412/422/451) rotation.
    pub fn key_count(&self, provider: &str) -> usize {
        self.credentials
            .get(provider)
            .map(|entries| entries.len())
            .unwrap_or(0)
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

    /// Effective strategy for `provider`: an explicit per-provider entry, else
    /// the configured default (stored under the empty key), else LeastLoaded.
    pub fn strategy(&self, provider: &str) -> SelectionStrategy {
        if let Some(strategy) = self.strategies.get(provider) {
            return *strategy.value();
        }
        if let Some(strategy) = self.strategies.get("") {
            return *strategy.value();
        }
        SelectionStrategy::default()
    }

    /// Set the selection strategy for a single provider.
    pub fn set_strategy(&self, provider: &str, strategy: SelectionStrategy) {
        self.strategies.insert(provider.to_string(), strategy);
    }

    /// Set the default strategy applied to every provider without an explicit
    /// override. Stored under the empty-string key so the field still derives
    /// `Default` (DashMap-only storage, no extra field type).
    pub fn set_default_strategy(&self, strategy: SelectionStrategy) {
        self.strategies.insert(String::new(), strategy);
    }

    /// Strategy-aware acquisition with no extra predicate.
    pub fn acquire(&self, provider: &str) -> Option<CredentialEntry> {
        self.acquire_where(provider, |_| true)
    }

    /// Strategy-aware acquisition: dispatches to least-loaded / round-robin /
    /// weighted-random based on the configured strategy, then applies
    /// `is_available` (cooldown / last-key / request-level exclusions).
    pub fn acquire_where<F>(&self, provider: &str, is_available: F) -> Option<CredentialEntry>
    where
        F: Fn(&str) -> bool,
    {
        match self.strategy(provider) {
            SelectionStrategy::LeastLoaded => {
                self.acquire_least_loaded_where(provider, is_available)
            }
            SelectionStrategy::RoundRobin => {
                self.acquire_sequential_where(provider, &is_available, false)
            }
            SelectionStrategy::WeightedRandom => {
                self.acquire_sequential_where(provider, &is_available, true)
            }
        }
    }

    /// Shared core for round-robin / weighted-random. Starts at a per-call
    /// offset (monotonic counter for round-robin, random for weighted-random)
    /// and scans forward (wrapping) for the first eligible key it can
    /// CAS-acquire. A few sweeps absorb contention so a momentarily-contended
    /// key is not mistaken for "all busy".
    fn acquire_sequential_where<F>(
        &self,
        provider: &str,
        is_available: &F,
        random_start: bool,
    ) -> Option<CredentialEntry>
    where
        F: Fn(&str) -> bool,
    {
        let entries = self.credentials.get(provider)?;
        let n = entries.len();
        if n == 0 {
            return None;
        }
        let start = if random_start {
            rand::thread_rng().gen_range(0..n)
        } else {
            // Round-robin start hint, read from the per-provider cursor. The
            // cursor is reduced modulo n immediately, so `start + offset < 2n`
            // cannot overflow. It is advanced to the actually-acquired index
            // below, keeping rotation balanced even when earlier keys were on
            // cooldown or at capacity.
            self.rr_counters
                .entry(provider.to_string())
                .or_default()
                .load(Ordering::Relaxed)
                % n
        };

        let sweeps = n.max(3);
        for _ in 0..sweeps {
            for offset in 0..n {
                let idx = (start + offset) % n;
                let entry = &entries[idx];
                let current = entry.current_requests.load(Ordering::Acquire);
                if current >= entry.concurrent_limit || !is_available(&entry.key) {
                    continue;
                }
                if entry
                    .current_requests
                    .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    // Advance the cursor just past the key we actually acquired,
                    // so rotation stays balanced when earlier keys were skipped.
                    // Best-effort under concurrency (Relaxed); weighted-random
                    // ignores the cursor entirely.
                    if !random_start
                        && let Some(cursor) = self.rr_counters.get(provider)
                    {
                        cursor.store((idx + 1) % n, Ordering::Relaxed);
                    }
                    return Some(entry.clone());
                }
                // Contended with another acquirer — try the next eligible key.
            }
        }
        None
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
    fn selection_strategy_parse_accepts_documented_aliases() {
        assert_eq!(
            SelectionStrategy::parse("least-loaded"),
            Some(SelectionStrategy::LeastLoaded)
        );
        assert_eq!(
            SelectionStrategy::parse("ROUND_ROBIN"),
            Some(SelectionStrategy::RoundRobin)
        );
        assert_eq!(
            SelectionStrategy::parse("weighted-random"),
            Some(SelectionStrategy::WeightedRandom)
        );
        assert_eq!(SelectionStrategy::parse("random"), Some(SelectionStrategy::WeightedRandom));
        assert_eq!(SelectionStrategy::parse("nope"), None);
    }

    #[test]
    fn default_strategy_is_least_loaded() {
        // With no strategy configured, acquire_where behaves exactly like the
        // historical least-loaded path (no behavior change unless opted in).
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string()],
            1,
        );
        assert_eq!(manager.strategy("openai"), SelectionStrategy::LeastLoaded);

        manager.increment("openai", "key-0");
        let selected = manager.acquire_where("openai", |_| true).unwrap();
        assert_eq!(selected.key, "key-1");
    }

    #[test]
    fn round_robin_cycles_through_keys_in_order() {
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string(), "key-2".to_string()],
            50,
        );
        manager.set_default_strategy(SelectionStrategy::RoundRobin);

        let a = manager.acquire_where("openai", |_| true).unwrap();
        let b = manager.acquire_where("openai", |_| true).unwrap();
        let c = manager.acquire_where("openai", |_| true).unwrap();
        // Permits are held (not dropped), so the monotonic cursor advances 0..1..2.
        assert_eq!(a.key, "key-0");
        assert_eq!(b.key, "key-1");
        assert_eq!(c.key, "key-2");
    }

    #[test]
    fn round_robin_skips_excluded_keys_without_drift() {
        // An earlier key is excluded (e.g. on cooldown). Round-robin must skip it
        // and resume rotation just past the key it actually acquires, not drift
        // toward the tail of the list.
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string(), "key-2".to_string()],
            50,
        );
        manager.set_default_strategy(SelectionStrategy::RoundRobin);

        let first = manager.acquire_where("openai", |k| k != "key-0").unwrap();
        assert_eq!(first.key, "key-1");
        manager.decrement("openai", &first.key);

        let second = manager.acquire_where("openai", |k| k != "key-0").unwrap();
        assert_eq!(second.key, "key-2");
        manager.decrement("openai", &second.key);

        // Wraps past the excluded key-0 back to key-1.
        let third = manager.acquire_where("openai", |k| k != "key-0").unwrap();
        assert_eq!(third.key, "key-1");
    }

    #[test]
    fn weighted_random_only_returns_eligible_keys_and_covers_all() {
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string(), "key-2".to_string()],
            50,
        );
        manager.set_default_strategy(SelectionStrategy::WeightedRandom);

        // Exclude key-1 via the predicate; every acquisition must skip it.
        // Release each permit so the concurrency counters don't saturate.
        for _ in 0..50 {
            let selected = manager
                .acquire_where("openai", |k| k != "key-1")
                .expect("an eligible key exists");
            assert_ne!(selected.key, "key-1");
            manager.decrement("openai", &selected.key);
        }

        // With no exclusions and enough samples, every key is reachable.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let key = manager.acquire_where("openai", |_| true).unwrap().key;
            manager.decrement("openai", &key);
            seen.insert(key);
        }
        assert!(seen.contains("key-0"));
        assert!(seen.contains("key-1"));
        assert!(seen.contains("key-2"));
    }

    #[test]
    fn per_provider_strategy_overrides_default() {
        let manager = CredentialManager::new();
        manager.register_keys(
            "openai".to_string(),
            vec!["key-0".to_string(), "key-1".to_string()],
            50,
        );
        manager.set_default_strategy(SelectionStrategy::RoundRobin);
        manager.set_strategy("openai", SelectionStrategy::WeightedRandom);

        assert_eq!(manager.strategy("openai"), SelectionStrategy::WeightedRandom);
        assert_eq!(manager.strategy("anthropic"), SelectionStrategy::RoundRobin);
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
