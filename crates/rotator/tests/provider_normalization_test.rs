use rotator::{normalize_model_ref, normalize_provider_id, public_model_id, strip_provider_prefix};

#[test]
fn normalizes_provider_aliases_to_canonical_ids() {
    assert_eq!(normalize_provider_id("nano-gpt"), "nanogpt");
    assert_eq!(normalize_provider_id("together"), "together");
    assert_eq!(normalize_provider_id("together_ai"), "together");
    assert_eq!(normalize_provider_id("together-ai"), "together");
}

#[test]
fn normalizes_model_refs_without_double_prefixing() {
    let normalized = normalize_model_ref("openai/gpt-4o", Some("openai"));
    assert_eq!(normalized.provider_id, "openai");
    assert_eq!(normalized.upstream_model, "gpt-4o");
    assert_eq!(normalized.public_model, "openai/gpt-4o");

    let alias = normalize_model_ref("nano-gpt/claude-sonnet-4", None);
    assert_eq!(alias.provider_id, "nanogpt");
    assert_eq!(alias.upstream_model, "claude-sonnet-4");
    assert_eq!(alias.public_model, "nanogpt/claude-sonnet-4");
}

#[test]
fn strips_matching_provider_or_alias_prefix_only() {
    assert_eq!(strip_provider_prefix("openai/gpt-5", "openai"), "gpt-5");
    assert_eq!(
        strip_provider_prefix("nano-gpt/gpt-4o", "nanogpt"),
        "gpt-4o"
    );
    assert_eq!(
        strip_provider_prefix("anthropic/claude-sonnet", "openai"),
        "anthropic/claude-sonnet"
    );
}

#[test]
fn public_model_ids_are_canonical_and_single_prefixed() {
    assert_eq!(public_model_id("openai", "gpt-4o"), "openai/gpt-4o");
    assert_eq!(public_model_id("openai", "openai/gpt-4o"), "openai/gpt-4o");
    assert_eq!(
        public_model_id("nano-gpt", "nano-gpt/gpt-4o"),
        "nanogpt/gpt-4o"
    );
}
