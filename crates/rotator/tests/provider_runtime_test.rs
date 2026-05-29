use rotator::{AuthType, ProviderDefinition, ProviderRegistry, RuntimeProviderKind};
use std::collections::HashMap;

#[test]
fn runtime_route_uses_registry_base_url_and_action() {
    let registry = ProviderRegistry::new();

    let route = registry
        .resolve_runtime_route("openai", "chat/completions")
        .expect("openai route");

    assert_eq!(route.provider_id, "openai");
    assert_eq!(route.kind, RuntimeProviderKind::Registry);
    assert_eq!(route.base_url, "https://api.openai.com/v1");
    assert_eq!(route.action, "chat/completions");
}

#[test]
fn runtime_route_applies_provider_specific_actions() {
    let registry = ProviderRegistry::new();

    let gemini = registry
        .resolve_runtime_route("gemini", "chat/completions")
        .expect("gemini route");
    assert_eq!(gemini.action, "chat/completions");

    let elysiver = registry
        .resolve_runtime_route("elysiver", "chat/completions")
        .expect("elysiver route");
    let colin = registry
        .resolve_runtime_route("colin", "chat/completions")
        .expect("colin route");

    assert_eq!(elysiver.action, "responses");
    assert_eq!(colin.action, "responses");
}

#[test]
fn runtime_route_supports_env_defined_unknown_providers() {
    let registry = ProviderRegistry::default();
    registry.register(ProviderDefinition {
        id: "custom".to_owned(),
        display_name: "custom".to_owned(),
        base_url: "https://custom.example/root".to_owned(),
        auth_type: AuthType::Bearer,
        model_patterns: vec![],
        endpoints: vec!["/chat/completions".to_owned()],
        features: vec!["chat".to_owned()],
        model_count: 0,
        timeout_secs: 60,
        default_headers: HashMap::new(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    });

    let route = registry
        .resolve_runtime_route("custom", "chat/completions")
        .expect("custom route");

    assert_eq!(route.kind, RuntimeProviderKind::Registry);
    assert_eq!(route.base_url, "https://custom.example/root");
    assert_eq!(route.action, "chat/completions");
}

#[test]
fn runtime_route_rejects_unregistered_providers() {
    let registry = ProviderRegistry::default();

    assert!(
        registry
            .resolve_runtime_route("missing", "chat/completions")
            .is_none()
    );
}
