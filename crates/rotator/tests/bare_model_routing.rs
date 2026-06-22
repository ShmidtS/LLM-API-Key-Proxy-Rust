//! Документированный divergence от Python-оригинала.
//!
//! Python требует строгий формат `provider/model` и отклоняет голые имена моделей.
//! Rust-порт намеренно ПРИНИМАЕТ голые имена и детерминированно маршрутизирует их
//! к провайдеру по префиксу модели (см. `prefix_provider_for_model`). Это удобнее
//! для клиентов и не создаёт ambiguity: каждое имя резолвится к ровно одному
//! провайдеру. Эти тесты фиксируют контракт, чтобы поведение не регрессировало.

use rotator::ProviderRegistry;

fn resolve(model: &str) -> Option<String> {
    let registry = ProviderRegistry::new();
    registry.resolve_provider_by_model(model)
}

#[test]
fn bare_openai_models_route_to_openai() {
    assert_eq!(resolve("gpt-4o").as_deref(), Some("openai"));
    assert_eq!(resolve("gpt-5-mini").as_deref(), Some("openai"));
    assert_eq!(resolve("o4-mini").as_deref(), Some("openai"));
}

#[test]
fn bare_anthropic_models_route_to_anthropic() {
    assert_eq!(
        resolve("claude-3-5-sonnet-20241022").as_deref(),
        Some("anthropic")
    );
}

#[test]
fn bare_xai_and_zai_models_route_deterministically() {
    assert_eq!(resolve("grok-2").as_deref(), Some("xai"));
    assert_eq!(resolve("glm-4.6").as_deref(), Some("zai"));
}

#[test]
fn explicit_prefix_still_wins_over_bare_inference() {
    // Явный provider/model имеет приоритет над выводом по имени.
    assert_eq!(resolve("openrouter/gpt-4o").as_deref(), Some("openrouter"));
}

#[test]
fn bare_google_publisher_models_route_to_openrouter() {
    // OpenRouter publisher namespaces (e.g. `google/gemma-...`) have no native
    // provider; they must route to OpenRouter instead of falling through to the
    // openai default. The request path resolves via `find_provider_for_model`,
    // which inspects compiled patterns — so this is the resolver under test.
    let registry = ProviderRegistry::new();
    assert_eq!(
        registry
            .find_provider_for_model("google/gemma-4-31b-it:free")
            .as_deref(),
        Some("openrouter")
    );
    // The native gemini path (`gemini-*`, no `google/` prefix) is unaffected.
    assert_eq!(resolve("gemini-2.5-pro").as_deref(), Some("gemini"));
}

#[test]
fn openrouter_routing_preserves_google_namespace_upstream() {
    // Routing to the `openrouter` provider must NOT strip the `google/` prefix:
    // OpenRouter needs the full `google/gemma-...:free` id. strip_provider_prefix
    // only removes a prefix that matches the provider id, so `google/` survives.
    assert_eq!(
        rotator::strip_provider_prefix("google/gemma-4-31b-it:free", "openrouter"),
        "google/gemma-4-31b-it:free"
    );
}
