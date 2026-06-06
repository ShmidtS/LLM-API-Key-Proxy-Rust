use std::collections::HashMap;

const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;
#[allow(dead_code)]
const DEFAULT_SAFETY_BUFFER: u32 = 1_000;
const MIN_MAX_TOKENS: u32 = 1;

/// Registry mapping model identifiers to their context window sizes.
///
/// Supports both exact model IDs and prefix matching for model families.
#[derive(Clone, Debug)]
pub struct ContextWindowRegistry {
    exact: HashMap<&'static str, u32>,
    prefixes: Vec<(&'static str, u32)>,
}

impl Default for ContextWindowRegistry {
    fn default() -> Self {
        let mut exact = HashMap::new();
        exact.insert("gpt-4o", 128_000);
        exact.insert("gpt-4o-mini", 128_000);
        exact.insert("gpt-4-turbo", 128_000);
        exact.insert("gpt-4-0613", 8_192);
        exact.insert("gpt-4-32k-0613", 32_768);
        exact.insert("gpt-3.5-turbo", 16_385);
        exact.insert("gpt-3.5-turbo-0125", 16_385);
        exact.insert("gpt-3.5-turbo-1106", 16_385);
        exact.insert("gpt-3.5-turbo-16k", 16_385);
        exact.insert("o1-preview", 128_000);
        exact.insert("o1-mini", 128_000);
        exact.insert("o1", 128_000);
        exact.insert("claude-3-5-sonnet-20241022", 200_000);
        exact.insert("claude-3-5-haiku-20241022", 200_000);
        exact.insert("claude-3-opus-20240229", 200_000);
        exact.insert("claude-3-haiku-20240307", 200_000);
        exact.insert("claude-opus-4-1", 200_000);
        exact.insert("claude-sonnet-4", 200_000);
        exact.insert("gemini-2.5-pro", 1_048_576);
        exact.insert("gemini-2.5-flash", 1_048_576);
        exact.insert("gemini-2.0-flash", 1_048_576);
        exact.insert("gemini-1.5-pro", 2_097_152);
        exact.insert("gemini-1.5-flash", 1_048_576);
        exact.insert("llama-3.1-405b-instruct", 128_000);
        exact.insert("llama-3.1-70b-instruct", 128_000);
        exact.insert("llama-3.1-8b-instruct", 128_000);
        exact.insert("mistral-large-latest", 128_000);
        exact.insert("mistral-small-latest", 32_000);
        exact.insert("codestral-latest", 32_000);
        exact.insert("command-r-plus", 128_000);
        exact.insert("command-r", 128_000);
        exact.insert("qwen-max", 32_768);
        exact.insert("qwen-plus", 131_072);
        exact.insert("qwen-turbo", 1_000_000);
        exact.insert("deepseek-chat", 64_000);
        exact.insert("deepseek-reasoner", 64_000);
        exact.insert("grok-4", 256_000);
        exact.insert("grok-3", 131_072);
        exact.insert("glm-4.5", 128_000);

        let prefixes = vec![
            ("gpt-4o", 128_000),
            ("gpt-4-turbo", 128_000),
            ("gpt-4-32k", 32_768),
            ("gpt-4", 8_192),
            ("gpt-3.5-turbo", 16_385),
            ("o1-preview", 128_000),
            ("o1-mini", 128_000),
            ("o1", 128_000),
            ("o3", 128_000),
            ("o4", 128_000),
            ("claude-3-5-sonnet", 200_000),
            ("claude-3-5-haiku", 200_000),
            ("claude-3-opus", 200_000),
            ("claude-3-haiku", 200_000),
            ("claude-opus-4", 200_000),
            ("claude-sonnet-4", 200_000),
            ("gemini-2.5-pro", 1_048_576),
            ("gemini-2.5-flash", 1_048_576),
            ("gemini-2.0-flash", 1_048_576),
            ("gemini-1.5-pro", 2_097_152),
            ("gemini-1.5-flash", 1_048_576),
            ("llama-3.1-", 128_000),
            ("mistral-large-", 128_000),
            ("mistral-small-", 32_000),
            ("codestral-", 32_000),
            ("command-r-plus", 128_000),
            ("command-r", 128_000),
            ("qwen-max", 32_768),
            ("qwen-plus", 131_072),
            ("qwen-turbo", 1_000_000),
            ("deepseek-chat", 64_000),
            ("deepseek-reasoner", 64_000),
            ("deepseek-", 64_000),
            ("grok-4", 256_000),
            ("grok-3", 131_072),
            ("grok-", 131_072),
            ("glm-4.5", 128_000),
            ("glm-", 128_000),
        ];

        Self { exact, prefixes }
    }
}

impl ContextWindowRegistry {
    /// Look up the context window for a model.
    ///
    /// Strips common provider prefixes before matching. Falls back to
    /// `DEFAULT_CONTEXT_WINDOW` if no entry is found.
    pub fn lookup(&self, model: &str) -> u32 {
        let bare = strip_provider_prefix(model);

        if let Some(&window) = self.exact.get(bare) {
            return window;
        }

        for (prefix, window) in &self.prefixes {
            if bare.starts_with(prefix) {
                return *window;
            }
        }

        DEFAULT_CONTEXT_WINDOW
    }
}

fn strip_provider_prefix(model: &str) -> &str {
    model
        .strip_prefix("openai/")
        .or_else(|| model.strip_prefix("azure/"))
        .or_else(|| model.strip_prefix("anthropic/"))
        .or_else(|| model.strip_prefix("gemini/"))
        .or_else(|| model.strip_prefix("google/"))
        .or_else(|| model.strip_prefix("meta/"))
        .or_else(|| model.strip_prefix("mistral/"))
        .or_else(|| model.strip_prefix("cohere/"))
        .or_else(|| model.strip_prefix("qwen/"))
        .or_else(|| model.strip_prefix("deepseek/"))
        .or_else(|| model.strip_prefix("xai/"))
        .or_else(|| model.strip_prefix("zai/"))
        .or_else(|| model.strip_prefix("nvidia/"))
        .or_else(|| model.strip_prefix("fireworks/"))
        .or_else(|| model.strip_prefix("openrouter/"))
        .or_else(|| model.strip_prefix("chutes/"))
        .or_else(|| model.strip_prefix("colin/"))
        .or_else(|| model.strip_prefix("elysiver/"))
        .or_else(|| model.strip_prefix("opencode/"))
        .or_else(|| model.strip_prefix("iflow/"))
        .or_else(|| model.strip_prefix("kilocode/"))
        .or_else(|| model.strip_prefix("nanogpt/"))
        .or_else(|| model.strip_prefix("firmware/"))
        .or_else(|| model.strip_prefix("antigravity/"))
        .or_else(|| model.strip_prefix("qwen_code/"))
        .unwrap_or(model)
}

/// Calculator that determines the safe `max_tokens` value for a request.
#[derive(Debug, Clone, Default)]
pub struct TokenCalculator {
    registry: ContextWindowRegistry,
}

impl TokenCalculator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Calculate the appropriate `max_tokens` for a model given the input size.
    ///
    /// * `model`         — model identifier (may include provider prefix).
    /// * `input_tokens`  — number of tokens already consumed by the prompt.
    /// * `existing_max`  — `max_tokens` explicitly requested by the caller, if any.
    /// * `safety_buffer` — tokens reserved for system overhead (default 1000).
    ///
    /// Returns the value that should be sent upstream. If the caller already
    /// requested a value, it is capped to the available headroom. If no value was
    /// requested, the full remaining headroom is used.
    pub fn calculate_max_tokens(
        &self,
        model: &str,
        input_tokens: u32,
        existing_max: Option<u32>,
        safety_buffer: u32,
    ) -> u32 {
        let context_window = self.registry.lookup(model);
        let available = context_window.saturating_sub(input_tokens).saturating_sub(safety_buffer);

        let max_tokens = match existing_max {
            Some(requested) => requested.min(available),
            None => available,
        };

        max_tokens.max(MIN_MAX_TOKENS)
    }
}

/// Convenience function for the common case.
pub fn calculate_max_tokens(
    model: &str,
    input_tokens: u32,
    existing_max: Option<u32>,
    safety_buffer: u32,
) -> u32 {
    let calculator = TokenCalculator::new();
    calculator.calculate_max_tokens(model, input_tokens, existing_max, safety_buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_max_tokens_for_known_model() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o", 1000, None, DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, 128_000 - 1000 - DEFAULT_SAFETY_BUFFER);
    }

    #[test]
    fn respects_existing_max_tokens_when_within_limit() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o", 1000, Some(5000), DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, 5000);
    }

    #[test]
    fn caps_existing_max_tokens_when_exceeds_limit() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o", 1000, Some(200_000), DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, 128_000 - 1000 - DEFAULT_SAFETY_BUFFER);
    }

    #[test]
    fn strips_provider_prefix_before_lookup() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("openai/gpt-4o", 1000, None, DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, 128_000 - 1000 - DEFAULT_SAFETY_BUFFER);
    }

    #[test]
    fn uses_prefix_fallback_for_unknown_variant() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o-2024-08-06", 1000, None, DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, 128_000 - 1000 - DEFAULT_SAFETY_BUFFER);
    }

    #[test]
    fn uses_default_context_window_for_unknown_model() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("unknown-model-v99", 1000, None, DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, DEFAULT_CONTEXT_WINDOW - 1000 - DEFAULT_SAFETY_BUFFER);
    }

    #[test]
    fn returns_at_least_one_token_when_headroom_exhausted() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o", 200_000, None, DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, MIN_MAX_TOKENS);
    }

    #[test]
    fn caps_existing_max_when_headroom_exhausted() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o", 200_000, Some(50_000), DEFAULT_SAFETY_BUFFER);
        assert_eq!(result, MIN_MAX_TOKENS);
    }

    #[test]
    fn handles_custom_safety_buffer() {
        let calc = TokenCalculator::new();
        let result = calc.calculate_max_tokens("gpt-4o", 1000, None, 500);
        assert_eq!(result, 128_000 - 1000 - 500);
    }

    #[test]
    fn context_window_claude_models() {
        let calc = TokenCalculator::new();
        assert_eq!(calc.calculate_max_tokens("claude-3-5-sonnet-20241022", 0, None, 0), 200_000);
        assert_eq!(calc.calculate_max_tokens("anthropic/claude-3-opus-20240229", 0, None, 0), 200_000);
    }

    #[test]
    fn context_window_gemini_models() {
        let calc = TokenCalculator::new();
        assert_eq!(calc.calculate_max_tokens("gemini-1.5-pro", 0, None, 0), 2_097_152);
        assert_eq!(calc.calculate_max_tokens("gemini-2.5-flash", 0, None, 0), 1_048_576);
    }

    #[test]
    fn context_window_gpt4_base() {
        let calc = TokenCalculator::new();
        assert_eq!(calc.calculate_max_tokens("gpt-4", 0, None, 0), 8_192);
        assert_eq!(calc.calculate_max_tokens("gpt-4-32k", 0, None, 0), 32_768);
    }

    #[test]
    fn convenience_function_works() {
        let result = calculate_max_tokens("gpt-4o-mini", 500, Some(10_000), 1000);
        assert_eq!(result, 10_000);
    }
}
