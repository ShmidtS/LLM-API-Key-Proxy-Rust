use crate::credentials::CredentialManager;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

static CREDENTIALS: LazyLock<CredentialManager> = LazyLock::new(CredentialManager::new);

pub fn export_credentials() -> Value {
    export_credentials_from(&CREDENTIALS)
}

pub fn import_credentials(data: Value) {
    import_credentials_into(&CREDENTIALS, data);
}

pub fn export_credentials_from(manager: &CredentialManager) -> Value {
    let mut providers = Map::new();

    for entry in manager.credentials.iter() {
        let credentials: Vec<_> = entry
            .value()
            .iter()
            .map(|credential| {
                json!({
                    "key": credential.key,
                    "provider": credential.provider,
                    "concurrent_limit": credential.concurrent_limit,
                })
            })
            .collect();
        providers.insert(entry.key().clone(), Value::Array(credentials));
    }

    Value::Object(providers)
}

pub fn import_credentials_into(manager: &CredentialManager, data: Value) {
    let Some(providers) = data.as_object() else {
        return;
    };

    for (provider, credentials) in providers {
        let Some(items) = credentials.as_array() else {
            continue;
        };

        let mut keys = Vec::new();
        let mut limit = 10;
        for item in items {
            if let Some(key) = item.as_str() {
                keys.push(key.to_string());
                continue;
            }

            let Some(object) = item.as_object() else {
                continue;
            };
            if let Some(key) = object.get("key").and_then(Value::as_str) {
                keys.push(key.to_string());
            }
            if let Some(concurrent_limit) = object.get("concurrent_limit").and_then(Value::as_u64) {
                limit = concurrent_limit as usize;
            }
        }

        if !keys.is_empty() {
            manager.register_keys(provider.clone(), keys, limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_manager_credentials() {
        let manager = CredentialManager::new();
        manager.register_keys("openai".to_string(), vec!["key-1".to_string()], 3);

        let data = export_credentials_from(&manager);

        assert_eq!(data["openai"][0]["key"], "key-1");
        assert_eq!(data["openai"][0]["provider"], "openai");
        assert_eq!(data["openai"][0]["concurrent_limit"], 3);
    }

    #[test]
    fn imports_object_credentials() {
        let manager = CredentialManager::new();

        import_credentials_into(
            &manager,
            json!({
                "openai": [{"key": "key-1", "concurrent_limit": 2}],
                "anthropic": ["key-2"]
            }),
        );

        assert_eq!(manager.credentials.get("openai").unwrap()[0].key, "key-1");
        assert_eq!(
            manager.credentials.get("openai").unwrap()[0].concurrent_limit,
            2
        );
        assert_eq!(manager.credentials.get("anthropic").unwrap()[0].key, "key-2");
    }

    #[test]
    fn global_import_then_export_round_trips() {
        import_credentials(json!({"test_global": [{"key": "global-key", "concurrent_limit": 1}]}));

        let data = export_credentials();

        assert_eq!(data["test_global"][0]["key"], "global-key");
    }
}
