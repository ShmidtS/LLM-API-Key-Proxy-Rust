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

pub fn estimate_cost(model: &str, input_tokens: usize, output_tokens: usize) -> f64 {
    let Some((_, input_cost, output_cost)) = COST_TABLE
        .iter()
        .find(|(prefix, _, _)| model.starts_with(prefix))
    else {
        return 0.0;
    };

    (input_tokens as f64 / 1_000.0 * input_cost) + (output_tokens as f64 / 1_000.0 * output_cost)
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
