#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedModelRef {
    pub original: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub public_model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderAlias {
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
}

pub const PROVIDER_ALIASES: &[ProviderAlias] = &[
    ProviderAlias {
        canonical: "nanogpt",
        aliases: &["nano-gpt"],
    },
    ProviderAlias {
        canonical: "together_ai",
        aliases: &["together", "together-ai"],
    },
];

pub fn normalize_provider_id(input: &str) -> String {
    let normalized = input.trim().to_ascii_lowercase();
    for alias in PROVIDER_ALIASES {
        if normalized == alias.canonical || alias.aliases.iter().any(|value| *value == normalized) {
            return alias.canonical.to_owned();
        }
    }
    normalized
}

pub fn normalize_model_ref(input: &str, default_provider: Option<&str>) -> NormalizedModelRef {
    let original = input.to_owned();
    let (provider_id, upstream_model) = input
        .split_once('/')
        .map(|(provider, model)| (normalize_provider_id(provider), model.to_owned()))
        .unwrap_or_else(|| {
            let provider = default_provider
                .map(normalize_provider_id)
                .unwrap_or_else(|| "openai".to_owned());
            (provider, input.to_owned())
        });
    let public_model = public_model_id(&provider_id, &upstream_model);

    NormalizedModelRef {
        original,
        provider_id,
        upstream_model,
        public_model,
    }
}

pub fn strip_provider_prefix(model: &str, provider: &str) -> String {
    let provider = normalize_provider_id(provider);
    if let Some((prefix, rest)) = model.split_once('/')
        && normalize_provider_id(prefix) == provider
    {
        return rest.to_owned();
    }
    model.to_owned()
}

pub fn public_model_id(provider: &str, upstream_model: &str) -> String {
    let provider = normalize_provider_id(provider);
    let upstream_model = strip_provider_prefix(upstream_model, &provider);
    format!("{provider}/{upstream_model}")
}
