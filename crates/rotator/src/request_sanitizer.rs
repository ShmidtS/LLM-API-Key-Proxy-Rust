use crate::provider_normalization::{normalize_provider_id, strip_provider_prefix};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizerContext {
    pub provider_id: String,
    pub model: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SanitizerAction {
    RemoveParam(&'static str),
    RenameParam {
        from: &'static str,
        to: &'static str,
    },
    ForceTemperatureZero,
    StripProviderPrefix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizerRule {
    pub provider: Option<&'static str>,
    pub model_prefixes: &'static [&'static str],
    pub endpoint: Option<&'static str>,
    pub actions: &'static [SanitizerAction],
}

const OPENAI_REASONING_ACTIONS: &[SanitizerAction] = &[SanitizerAction::RemoveParam("temperature")];
const STRIP_PREFIX_ACTIONS: &[SanitizerAction] = &[SanitizerAction::StripProviderPrefix];
const ANTHROPIC_ACTIONS: &[SanitizerAction] = &[
    SanitizerAction::StripProviderPrefix,
    SanitizerAction::RemoveParam("stream_options"),
];
const GEMINI_ACTIONS: &[SanitizerAction] = &[
    SanitizerAction::StripProviderPrefix,
    SanitizerAction::RemoveParam("stream_options"),
    SanitizerAction::RemoveParam("logprobs"),
    SanitizerAction::RemoveParam("top_logprobs"),
];

pub const SANITIZER_RULES: &[SanitizerRule] = &[
    SanitizerRule {
        provider: None,
        model_prefixes: &[],
        endpoint: None,
        actions: STRIP_PREFIX_ACTIONS,
    },
    SanitizerRule {
        provider: Some("openai"),
        model_prefixes: &["gpt-5", "o1", "o3", "o4"],
        endpoint: None,
        actions: OPENAI_REASONING_ACTIONS,
    },
    SanitizerRule {
        provider: Some("anthropic"),
        model_prefixes: &[],
        endpoint: None,
        actions: ANTHROPIC_ACTIONS,
    },
    SanitizerRule {
        provider: Some("gemini"),
        model_prefixes: &[],
        endpoint: None,
        actions: GEMINI_ACTIONS,
    },
];

pub fn sanitize_request(context: &SanitizerContext, body: &mut serde_json::Value) {
    let provider_id = normalize_provider_id(&context.provider_id);
    for rule in SANITIZER_RULES {
        if !rule_matches(rule, &provider_id, &context.model, &context.endpoint) {
            continue;
        }
        for action in rule.actions {
            apply_action(action, context, &provider_id, body);
        }
    }
}

fn rule_matches(rule: &SanitizerRule, provider_id: &str, model: &str, endpoint: &str) -> bool {
    if let Some(provider) = rule.provider
        && normalize_provider_id(provider) != provider_id
    {
        return false;
    }
    if let Some(rule_endpoint) = rule.endpoint
        && rule_endpoint.trim_start_matches('/') != endpoint.trim_start_matches('/')
    {
        return false;
    }
    if rule.model_prefixes.is_empty() {
        return true;
    }
    let upstream_model = strip_provider_prefix(model, provider_id);
    rule.model_prefixes
        .iter()
        .any(|prefix| upstream_model.starts_with(prefix))
}

fn apply_action(
    action: &SanitizerAction,
    context: &SanitizerContext,
    provider_id: &str,
    body: &mut serde_json::Value,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    match action {
        SanitizerAction::RemoveParam(name) => {
            object.remove(*name);
        }
        SanitizerAction::RenameParam { from, to } => {
            if let Some(value) = object.remove(*from) {
                object.insert((*to).to_owned(), value);
            }
        }
        SanitizerAction::ForceTemperatureZero => {
            object.insert("temperature".to_owned(), serde_json::json!(0.0));
        }
        SanitizerAction::StripProviderPrefix => {
            if provider_id == "openai" && context.endpoint == "responses" {
                return;
            }
            if let Some(model) = object.get("model").and_then(serde_json::Value::as_str) {
                let stripped = strip_provider_prefix(model, provider_id);
                if stripped != model {
                    object.insert("model".to_owned(), serde_json::Value::String(stripped));
                }
            }
        }
    }
}
