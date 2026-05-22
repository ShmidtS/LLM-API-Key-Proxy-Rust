use dashmap::DashMap;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct CooldownEntry {
    pub provider: String,
    pub key: String,
    pub expires_at: Instant,
}

#[derive(Debug, Default)]
pub struct CooldownManager {
    cooldowns: DashMap<(String, String), CooldownEntry>,
    provider_cooldowns: DashMap<String, Instant>,
}

impl CooldownManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_cooldown(&self, provider: &str, key: &str, duration: Duration) {
        let entry = CooldownEntry {
            provider: provider.to_owned(),
            key: key.to_owned(),
            expires_at: Instant::now() + duration,
        };

        self.cooldowns
            .insert((provider.to_owned(), key.to_owned()), entry);
    }

    pub fn add_provider_cooldown(&self, provider: &str, duration: Duration) {
        self.provider_cooldowns
            .insert(provider.to_owned(), Instant::now() + duration);
    }

    pub fn is_provider_available(&self, provider: &str) -> bool {
        if let Some(expires_at) = self.provider_cooldowns.get(provider) {
            if Instant::now() < *expires_at {
                return false;
            }
        } else {
            return true;
        }

        self.provider_cooldowns.remove(provider);
        true
    }

    pub fn is_available(&self, provider: &str, key: &str) -> bool {
        if !self.is_provider_available(provider) {
            return false;
        }

        let cooldown_key = (provider.to_owned(), key.to_owned());

        if let Some(entry) = self.cooldowns.get(&cooldown_key) {
            if Instant::now() < entry.expires_at {
                return false;
            }
        } else {
            return true;
        }

        self.cooldowns.remove(&cooldown_key);
        true
    }

    pub fn remove_cooldown(&self, provider: &str, key: &str) {
        self.cooldowns
            .remove(&(provider.to_owned(), key.to_owned()));
    }

    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        self.cooldowns.retain(|_, entry| entry.expires_at > now);
    }

    pub fn get_active_cooldowns(&self) -> Vec<(String, String, Duration)> {
        let now = Instant::now();
        self.cooldowns
            .iter()
            .filter_map(|entry| {
                let cooldown = entry.value();
                if cooldown.expires_at > now {
                    Some((
                        cooldown.provider.clone(),
                        cooldown.key.clone(),
                        cooldown.expires_at.duration_since(now),
                    ))
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn key_is_unavailable_during_cooldown() {
        let manager = CooldownManager::new();

        manager.add_cooldown("openai", "key-1", Duration::from_secs(60));

        assert!(!manager.is_available("openai", "key-1"));
    }

    #[tokio::test]
    async fn key_becomes_available_after_cooldown_expires() {
        let manager = CooldownManager::new();

        manager.add_cooldown("openai", "key-1", Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(manager.is_available("openai", "key-1"));
    }

    #[test]
    fn never_added_key_is_available() {
        let manager = CooldownManager::new();

        assert!(manager.is_available("openai", "key-1"));
    }

    #[test]
    fn remove_cooldown_makes_key_available() {
        let manager = CooldownManager::new();

        manager.add_cooldown("openai", "key-1", Duration::from_secs(60));
        manager.remove_cooldown("openai", "key-1");

        assert!(manager.is_available("openai", "key-1"));
    }

    #[test]
    fn provider_is_unavailable_during_cooldown() {
        let manager = CooldownManager::new();

        manager.add_provider_cooldown("openai", Duration::from_secs(60));

        assert!(!manager.is_provider_available("openai"));
        assert!(!manager.is_available("openai", "key-1"));
    }

    #[tokio::test]
    async fn provider_becomes_available_after_cooldown_expires() {
        let manager = CooldownManager::new();

        manager.add_provider_cooldown("openai", Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(manager.is_provider_available("openai"));
        assert!(manager.is_available("openai", "key-1"));
    }

    #[test]
    fn provider_cooldown_does_not_block_other_providers() {
        let manager = CooldownManager::new();

        manager.add_provider_cooldown("openai", Duration::from_secs(60));

        assert!(manager.is_available("anthropic", "key-1"));
    }

    #[test]
    fn get_active_cooldowns_returns_remaining_time() {
        let manager = CooldownManager::new();

        manager.add_cooldown("openai", "key-1", Duration::from_secs(60));
        let active = manager.get_active_cooldowns();

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, "openai");
        assert_eq!(active[0].1, "key-1");
        assert!(active[0].2 <= Duration::from_secs(60));
        assert!(active[0].2 > Duration::ZERO);
    }
}
