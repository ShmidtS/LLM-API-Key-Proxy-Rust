use rotator::{ProviderRegistry, public_model_id};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn unknown_provider_api_base_registers_openai_compatible_provider() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("ACME_API_BASE", "https://acme.example/v1");
        std::env::set_var("ACME_MODELS", " model-a,model-b, ,model-c ");
    }

    let mut registry = ProviderRegistry::new();
    registry.load_from_env();

    let provider = registry.get("acme").expect("dynamic provider exists");
    assert_eq!(provider.base_url, "https://acme.example/v1");
    assert_eq!(provider.endpoints, vec!["/chat/completions"]);
    assert!(provider.features.contains(&"chat".to_owned()));
    assert_eq!(registry.get_static_models("acme"), Vec::<String>::new());
    assert_eq!(
        registry.resolve_provider_by_model("acme/model-a"),
        Some("acme".to_owned())
    );
    assert_eq!(public_model_id("acme", "model-a"), "acme/model-a");

    unsafe {
        std::env::remove_var("ACME_API_BASE");
        std::env::remove_var("ACME_MODELS");
    }
}

#[test]
fn proxy_provider_models_alias_is_preserved() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("PROXY_WIDGET_URL", "https://widget.example/v1");
        std::env::set_var("PROXY_WIDGET_MODELS", "^legacy-.*");
        std::env::set_var("WIDGET_MODELS", "widget-a,widget-b");
    }

    let mut registry = ProviderRegistry::new();
    registry.load_from_env();

    let provider = registry.get("widget").expect("widget provider exists");
    assert_eq!(provider.model_patterns, vec!["^legacy-.*".to_owned()]);
    assert_eq!(
        registry.resolve_provider_by_model("widget/widget-a"),
        Some("widget".to_owned())
    );

    unsafe {
        std::env::remove_var("PROXY_WIDGET_URL");
        std::env::remove_var("PROXY_WIDGET_MODELS");
        std::env::remove_var("WIDGET_MODELS");
    }
}
