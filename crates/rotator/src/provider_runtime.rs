#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProviderKind {
    Registry,
    LegacyModule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProviderRoute {
    pub provider_id: String,
    pub kind: RuntimeProviderKind,
    pub base_url: String,
    pub action: String,
}

pub fn normalize_upstream_url(base_url: &str, action: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    let action = action.trim_start_matches('/');

    if base_url.ends_with("/v1")
        || base_url.ends_with("/v1/openai")
        || base_url.contains(action)
        || action.contains(':')
    {
        return format!("{base_url}/{action}");
    }

    if let Ok(url) = reqwest::Url::parse(base_url) {
        let path = url.path();
        if path.len() > 1 {
            return format!("{base_url}/{action}");
        }
    }

    format!("{base_url}/v1/{action}")
}
