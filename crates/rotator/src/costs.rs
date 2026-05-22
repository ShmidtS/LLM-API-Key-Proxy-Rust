use serde::Serialize;

const COST_TABLE: &[(&str, f64, f64)] = &[
    ("gpt-4o-mini", 0.15, 0.6),
    ("gpt-4o", 2.5, 10.0),
    ("claude-3-5-sonnet", 3.0, 15.0),
    ("claude-3-haiku", 0.25, 1.25),
    ("gemini-1.5-flash", 0.075, 0.3),
    ("gemini-1.5-pro", 1.25, 5.0),
    ("qwen-max", 5.0, 10.0),
    ("deepseek-chat", 0.14, 0.28),
];

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CostBreakdown {
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub cache_creation_cost: f64,
    pub total_cost: f64,
}

pub fn estimate_cost(model: &str, input_tokens: usize, output_tokens: usize) -> f64 {
    estimate_cost_breakdown(model, input_tokens as u64, output_tokens as u64, None, None)
        .map(|breakdown| breakdown.total_cost)
        .unwrap_or(0.0)
}

pub fn estimate_cost_breakdown(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
) -> Option<CostBreakdown> {
    let (_, input_cost_per_1k, output_cost_per_1k) = COST_TABLE
        .iter()
        .find(|(prefix, _, _)| model.starts_with(prefix))?;

    let input_cost = prompt_tokens as f64 / 1_000.0 * input_cost_per_1k;
    let output_cost = completion_tokens as f64 / 1_000.0 * output_cost_per_1k;
    let cache_read_cost = cache_read_tokens.unwrap_or(0) as f64 / 1_000.0 * input_cost_per_1k;
    let cache_creation_cost =
        cache_creation_tokens.unwrap_or(0) as f64 / 1_000.0 * input_cost_per_1k;
    let total_cost = input_cost + output_cost + cache_read_cost + cache_creation_cost;

    Some(CostBreakdown {
        input_cost,
        output_cost,
        cache_read_cost,
        cache_creation_cost,
        total_cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_known_model_prefix_cost() {
        let cost = estimate_cost("gpt-4o-2024-08-06", 1_000, 2_000);

        assert_eq!(cost, 22.5);
    }

    #[test]
    fn prefers_more_specific_prefix() {
        let cost = estimate_cost("gpt-4o-mini-2024-07-18", 1_000, 1_000);

        assert_eq!(cost, 0.75);
    }

    #[test]
    fn returns_zero_for_unknown_model() {
        assert_eq!(estimate_cost("unknown-model", 1_000, 1_000), 0.0);
    }
}
