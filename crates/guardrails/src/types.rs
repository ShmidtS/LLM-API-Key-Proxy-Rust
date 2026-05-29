use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteKind {
    ChatCompletions,
    AnthropicMessages,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GuardrailMode {
    #[default]
    Off,
    Observe,
    Enforce,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailRequest {
    pub route: RouteKind,
    pub provider: String,
    pub upstream_path: String,
    pub model: String,
    pub body: serde_json::Value,
    pub stream: bool,
    pub schema_hint: Option<SchemaHint>,
    pub step_policy: Option<StepPolicy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaHint {
    ToolCalls,
    JsonMode,
    JsonSchema(serde_json::Value),
    StepCompletion(Vec<String>, Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepPolicy {
    pub required_steps: Vec<String>,
    pub before_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardrailOutcome {
    pub body: serde_json::Value,
    pub warnings: Vec<GuardrailWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailWarning {
    pub code: String,
    pub message: String,
    pub severity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub field: String,
    pub reason: String,
    pub severity: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GuardrailTrace {
    pub issues: Vec<ValidationIssue>,
    pub warnings: Vec<GuardrailWarning>,
    pub actions_taken: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GuardrailDecision {
    Accept,
    RetryWithNudge {
        nudge_message: serde_json::Value,
        reason: String,
    },
    CompactAndRetry {
        compacted_body: serde_json::Value,
        reason: String,
    },
    Reject {
        client_error: String,
    },
    Abort {
        internal_error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RescueCandidate {
    pub body: serde_json::Value,
    pub repaired_fields: Vec<String>,
    pub remaining_issues: Vec<ValidationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ValidationOptions {
    pub validate_tool_calls: bool,
    pub validate_json_mode: bool,
    pub validate_schema: bool,
    pub validate_steps: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub ok: bool,
    pub violations: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn ok() -> Self {
        Self {
            ok: true,
            violations: Vec::new(),
        }
    }

    pub fn from_violations(violations: Vec<ValidationIssue>) -> Self {
        Self {
            ok: violations.is_empty(),
            violations,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenBudget {
    pub max_context_tokens: usize,
    pub compact_above_ratio: f32,
    pub reserve_output_tokens: usize,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            compact_above_ratio: 0.8,
            reserve_output_tokens: 4_096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CompactionResult {
    Unchanged,
    Compacted {
        body: serde_json::Value,
        summary_message: serde_json::Value,
        removed_messages: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamErrorSummary {
    pub status_code: Option<u16>,
    pub provider_error_message: Option<String>,
    pub error_kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validation_report_ok_sets_empty_state() {
        let report = ValidationReport::ok();
        assert!(report.ok);
        assert!(report.violations.is_empty());
    }

    #[test]
    fn request_serializes_schema_hint() {
        let request = GuardrailRequest {
            route: RouteKind::ChatCompletions,
            provider: "openai".into(),
            upstream_path: "/v1/chat/completions".into(),
            model: "gpt".into(),
            body: json!({"messages": []}),
            stream: false,
            schema_hint: Some(SchemaHint::JsonMode),
            step_policy: None,
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["schema_hint"], json!("JsonMode"));
    }
}
