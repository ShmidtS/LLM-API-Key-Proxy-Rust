use std::time::Instant;

use crate::types::{GuardrailAttempt, GuardrailTrace, RouteKind, StepPolicy};

#[derive(Debug, Clone)]
pub struct GuardrailContext {
    pub request_id: String,
    pub route: RouteKind,
    pub started_at: Instant,
    pub trace: GuardrailTrace,
    pub retry_budget: RetryBudget,
    pub redaction: RedactionPolicy,
    pub step_policy: Option<StepPolicy>,
    pub attempt: GuardrailAttempt,
    pub request: Option<crate::types::GuardrailRequest>,
}

impl GuardrailContext {
    pub fn new(request_id: String, route: RouteKind, max_retries: u32) -> Self {
        Self {
            request_id,
            route,
            started_at: Instant::now(),
            trace: GuardrailTrace::default(),
            retry_budget: RetryBudget {
                used: 0,
                max: max_retries,
            },
            redaction: RedactionPolicy::default(),
            step_policy: None,
            attempt: GuardrailAttempt::default(),
            request: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    pub used: u32,
    pub max: u32,
}

impl RetryBudget {
    pub fn try_consume(&mut self) -> bool {
        if self.used >= self.max {
            return false;
        }
        self.used += 1;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RedactionPolicy {
    pub include_prompt_text: bool,
    pub include_tool_arguments: bool,
}

#[derive(Debug, Clone)]
pub struct CompactEvent {
    pub step_index: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub budget_tokens: usize,
    pub messages_before: usize,
    pub messages_after: usize,
    pub phase_reached: usize,
}

/// Callback при срабатывании порога контекста: (tokens, budget, ratio) -> опц. сообщение.
pub type ContextThresholdCallback =
    std::sync::Arc<dyn Fn(usize, usize, f32) -> Option<String> + Send + Sync>;

pub struct ContextManager<S: crate::compaction::CompactionStrategy> {
    strategy: S,
    budget_tokens: usize,
    last_known_tokens: Option<usize>,
    fired_thresholds: Vec<f32>,
    context_thresholds: Vec<f32>,
    on_compact: Option<std::sync::Arc<dyn Fn(CompactEvent) + Send + Sync>>,
    on_context_threshold: Option<ContextThresholdCallback>,
}

impl<S: crate::compaction::CompactionStrategy> ContextManager<S> {
    pub fn new(strategy: S, budget_tokens: usize) -> Self {
        Self {
            strategy,
            budget_tokens,
            last_known_tokens: None,
            fired_thresholds: Vec::new(),
            context_thresholds: Vec::new(),
            on_compact: None,
            on_context_threshold: None,
        }
    }

    pub fn with_thresholds<F>(
        strategy: S,
        budget_tokens: usize,
        thresholds: Vec<f32>,
        on_threshold: F,
    ) -> Self
    where
        F: Fn(usize, usize, f32) -> Option<String> + Send + Sync + 'static,
    {
        Self {
            strategy,
            budget_tokens,
            last_known_tokens: None,
            fired_thresholds: Vec::new(),
            context_thresholds: thresholds,
            on_compact: None,
            on_context_threshold: Some(std::sync::Arc::new(on_threshold)),
        }
    }

    pub fn with_compact_callback<F>(strategy: S, budget_tokens: usize, on_compact: F) -> Self
    where
        F: Fn(CompactEvent) + Send + Sync + 'static,
    {
        Self {
            strategy,
            budget_tokens,
            last_known_tokens: None,
            fired_thresholds: Vec::new(),
            context_thresholds: Vec::new(),
            on_compact: Some(std::sync::Arc::new(on_compact)),
            on_context_threshold: None,
        }
    }

    pub fn update_token_count(&mut self, total_tokens: usize) {
        self.last_known_tokens = Some(total_tokens);
    }

    pub fn estimate_tokens(&self, messages: &[serde_json::Value]) -> usize {
        if let Some(known) = self.last_known_tokens {
            return known;
        }
        messages
            .iter()
            .map(|m| {
                m.get("content")
                    .and_then(serde_json::Value::as_str)
                    .map(|s| s.chars().count().div_ceil(4).max(1))
                    .unwrap_or(1)
            })
            .sum()
    }

    pub fn check_thresholds(&mut self, messages: &[serde_json::Value]) -> Option<String> {
        let on_threshold = self.on_context_threshold.as_ref()?;
        if self.budget_tokens == 0 {
            return None;
        }
        let tokens = self.estimate_tokens(messages);
        let pct = tokens as f32 / self.budget_tokens as f32;

        self.fired_thresholds.retain(|&t| pct >= t);

        let mut highest_crossed: Option<f32> = None;
        for &threshold in &self.context_thresholds {
            if pct >= threshold && !self.fired_thresholds.contains(&threshold) {
                highest_crossed = Some(threshold);
            }
        }

        if let Some(threshold) = highest_crossed {
            self.fired_thresholds.push(threshold);
            return on_threshold(tokens, self.budget_tokens, pct);
        }
        None
    }

    pub fn maybe_compact(
        &mut self,
        messages: &[serde_json::Value],
        step_index: usize,
        step_hint: &str,
    ) -> Vec<serde_json::Value> {
        let tokens_before = self.estimate_tokens(messages);
        let (result, phase) = self
            .strategy
            .compact(messages, self.budget_tokens, step_hint);
        if phase == 0 {
            return result;
        }
        let tokens_after = self.estimate_tokens(&result);
        if let Some(on_compact) = &self.on_compact {
            let event = CompactEvent {
                step_index,
                tokens_before,
                tokens_after,
                budget_tokens: self.budget_tokens,
                messages_before: messages.len(),
                messages_after: result.len(),
                phase_reached: phase,
            };
            on_compact(event);
        }
        result
    }
}
