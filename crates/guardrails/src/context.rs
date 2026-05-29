use std::time::Instant;

use crate::types::{GuardrailTrace, RouteKind};

#[derive(Debug, Clone)]
pub struct GuardrailContext {
    pub request_id: String,
    pub route: RouteKind,
    pub started_at: Instant,
    pub trace: GuardrailTrace,
    pub retry_budget: RetryBudget,
    pub redaction: RedactionPolicy,
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
