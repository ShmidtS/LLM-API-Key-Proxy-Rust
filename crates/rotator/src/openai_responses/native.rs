use super::request::ResponsesEndpoint;

pub trait ResponsesCapabilityResolver: Send + Sync {
    fn endpoint_for(&self, provider: &str, model: &str) -> ResponsesEndpoint;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultResponsesCapabilityResolver;

impl ResponsesCapabilityResolver for DefaultResponsesCapabilityResolver {
    fn endpoint_for(&self, provider: &str, model: &str) -> ResponsesEndpoint {
        let model = model
            .split_once('/')
            .map(|(_, stripped)| stripped)
            .unwrap_or(model);
        if provider == "openai" && (model.starts_with("gpt-5") || model.starts_with("o4")) {
            ResponsesEndpoint::NativeResponses
        } else {
            ResponsesEndpoint::ChatCompletionsEmulation
        }
    }
}
