use crate::compaction::{ContextCompactor, DefaultContextCompactor};
use crate::config::GuardrailsConfig;
use crate::error::GuardrailError;
use crate::nudge::{DefaultRetryNudger, RetryNudger};
use crate::recovery::{DefaultErrorRecovery, ErrorRecovery, RecoveryAction};
use crate::streaming::{NoOpStreamValidator, StreamValidator};
use crate::tool_rescue::{DefaultToolCallRescuer, ToolCallRescuer};
use crate::types::{
    CompactionResult, GuardrailDecision, GuardrailMode, GuardrailRequest, SchemaHint,
    ValidationOptions, ValidationReport,
};
use crate::validation::{DefaultResponseValidator, ResponseValidator};
use serde_json::Value;
use tracing::{debug, warn};

pub struct GuardrailsEngine {
    config: GuardrailsConfig,
    validator: Box<dyn ResponseValidator>,
    rescuer: Box<dyn ToolCallRescuer>,
    nudger: Box<dyn RetryNudger>,
    recovery: Box<dyn ErrorRecovery>,
    compactor: Box<dyn ContextCompactor>,
    stream_validator: Box<dyn StreamValidator>,
}

impl std::fmt::Debug for GuardrailsEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardrailsEngine")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl GuardrailsEngine {
    pub fn new(config: GuardrailsConfig) -> Self {
        Self {
            config,
            validator: Box::new(DefaultResponseValidator),
            rescuer: Box::new(DefaultToolCallRescuer),
            nudger: Box::new(DefaultRetryNudger),
            recovery: Box::new(DefaultErrorRecovery),
            compactor: Box::new(DefaultContextCompactor),
            stream_validator: Box::new(NoOpStreamValidator),
        }
    }

    pub fn with_components(
        config: GuardrailsConfig,
        validator: Box<dyn ResponseValidator>,
        rescuer: Box<dyn ToolCallRescuer>,
        nudger: Box<dyn RetryNudger>,
        recovery: Box<dyn ErrorRecovery>,
        compactor: Box<dyn ContextCompactor>,
        stream_validator: Box<dyn StreamValidator>,
    ) -> Self {
        Self {
            config,
            validator,
            rescuer,
            nudger,
            recovery,
            compactor,
            stream_validator,
        }
    }

    pub fn config(&self) -> &GuardrailsConfig {
        &self.config
    }

    pub fn evaluate(
        &self,
        request: &GuardrailRequest,
        response: &Value,
    ) -> Result<GuardrailDecision, GuardrailError> {
        let route_config = self.config.route_config(&request.route);
        if route_config.mode == GuardrailMode::Off {
            return Ok(GuardrailDecision::Accept);
        }

        let options = ValidationOptions {
            validate_tool_calls: route_config.validate_tool_calls,
            validate_json_mode: route_config.validate_json_mode,
            validate_schema: route_config.validate_schema,
            validate_steps: route_config.validate_steps,
        };
        let report = self.validator.validate(request, response, &options)?;
        if report.ok {
            return Ok(GuardrailDecision::Accept);
        }

        warn!(
            violations = report.violations.len(),
            details = ?report.violations,
            "guardrail validation failed"
        );

        if route_config.mode == GuardrailMode::Observe {
            return Ok(GuardrailDecision::Accept);
        }

        if route_config.rescue_tool_calls
            && has_tool_call_violation(&report)
            && let Some(candidate) = self.rescuer.rescue(response)?
        {
            if candidate.remaining_issues.is_empty() && !candidate.repaired_fields.is_empty() {
                debug!(fields = ?candidate.repaired_fields, "guardrail rescued response");
                return Ok(GuardrailDecision::CompactAndRetry {
                    compacted_body: candidate.body,
                    reason: "repaired malformed tool calls".into(),
                });
            }
            debug!(issues = ?candidate.remaining_issues, "guardrail tool rescue skipped");
        }

        if route_config.retry_with_nudge && self.config.max_guardrail_retries > 0 {
            let nudge_message = self.nudger.nudge_message(&report.violations)?;
            return Ok(GuardrailDecision::RetryWithNudge {
                nudge_message,
                reason: retry_reason(&report),
            });
        }

        let action = self.recovery.recover_validation(&report, &self.config)?;
        debug!(?action, "guardrail recovery decision");

        match action {
            RecoveryAction::RetrySameProvider => {
                let nudge_message = self.nudger.nudge_message(&report.violations)?;
                Ok(GuardrailDecision::RetryWithNudge {
                    nudge_message,
                    reason: retry_reason(&report),
                })
            }
            RecoveryAction::RetryFallbackProvider | RecoveryAction::RetryModelSwap => {
                debug!(?action, "guardrail recovery action is not supported in P0");
                Ok(GuardrailDecision::Reject {
                    client_error: sanitized_client_error(),
                })
            }
            RecoveryAction::GiveUp => Ok(GuardrailDecision::Reject {
                client_error: sanitized_client_error(),
            }),
        }
    }

    pub fn preprocess(
        &self,
        request: &GuardrailRequest,
    ) -> Result<GuardrailRequest, GuardrailError> {
        let route_config = self.config.route_config(&request.route);
        if route_config.mode == GuardrailMode::Off && !self.config.context_compaction.enabled {
            return Ok(request.clone());
        }

        let mut processed = request.clone();
        match self
            .compactor
            .compact(&processed, &self.config.context_compaction)?
        {
            CompactionResult::Unchanged => {}
            CompactionResult::Compacted { body, .. } => {
                processed.body = body;
            }
        }

        if route_config.validate_steps {
            apply_step_enforcement(&mut processed);
        }

        let mut trace = crate::types::GuardrailTrace::default();
        self.stream_validator.finish(&mut trace)?;

        Ok(processed)
    }
}

impl Default for GuardrailsEngine {
    fn default() -> Self {
        Self::new(GuardrailsConfig::default())
    }
}

fn has_tool_call_violation(report: &ValidationReport) -> bool {
    report
        .violations
        .iter()
        .any(|violation| violation.field.contains("tool_calls"))
}

fn retry_reason(report: &ValidationReport) -> String {
    report
        .violations
        .first()
        .map(|issue| issue.reason.clone())
        .unwrap_or_else(|| "response failed validation".into())
}

fn sanitized_client_error() -> String {
    "response failed guardrail validation".into()
}

fn apply_step_enforcement(request: &mut GuardrailRequest) {
    let Some(policy) = &request.step_policy else {
        return;
    };

    let instruction = if policy.before_steps.len() >= 2 {
        format!(
            "Guardrail step policy: complete required steps [{}]. Preserve ordering: {}.",
            policy.required_steps.join(", "),
            policy.before_steps.join(" before ")
        )
    } else {
        format!(
            "Guardrail step policy: complete required steps [{}].",
            policy.required_steps.join(", ")
        )
    };

    if let Some(messages) = request
        .body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
    {
        messages.insert(
            0,
            serde_json::json!({
                "role": "system",
                "content": instruction,
            }),
        );
    } else {
        request.body["guardrail_step_policy"] = serde_json::json!({
            "required_steps": policy.required_steps,
            "before_steps": policy.before_steps,
            "instruction": instruction,
        });
    }

    if request.schema_hint.is_none() {
        request.schema_hint = Some(SchemaHint::StepCompletion(
            policy.required_steps.clone(),
            policy.before_steps.clone(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GuardrailsConfig, RecoveryConfig, RouteGuardrailConfig};
    use crate::types::{RouteKind, StepPolicy};
    use serde_json::json;

    fn request() -> GuardrailRequest {
        GuardrailRequest {
            route: RouteKind::ChatCompletions,
            provider: "openai".into(),
            upstream_path: "/v1/chat/completions".into(),
            model: "gpt".into(),
            body: json!({"messages": [{"role":"user","content":"answer"}]}),
            stream: false,
            schema_hint: Some(SchemaHint::JsonMode),
            step_policy: None,
        }
    }

    #[test]
    fn preprocess_returns_clone_when_disabled() {
        let engine = GuardrailsEngine::default();
        let original = request();
        let processed = engine.preprocess(&original).unwrap();
        assert_eq!(processed.body, original.body);
    }

    #[test]
    fn preprocess_adds_step_policy_message() {
        let mut config = GuardrailsConfig::default();
        config.chat_completions = RouteGuardrailConfig {
            mode: GuardrailMode::Enforce,
            validate_steps: true,
            ..RouteGuardrailConfig::default()
        };
        let engine = GuardrailsEngine::new(config);
        let mut original = request();
        original.step_policy = Some(StepPolicy {
            required_steps: vec!["plan".into()],
            before_steps: vec!["plan".into(), "final".into()],
        });
        let processed = engine.preprocess(&original).unwrap();
        assert_eq!(processed.body["messages"][0]["role"], "system");
    }

    #[test]
    fn evaluate_uses_route_validation_flags_without_hint() {
        let mut config = GuardrailsConfig::default();
        config.chat_completions = RouteGuardrailConfig {
            mode: GuardrailMode::Enforce,
            validate_json_mode: true,
            ..RouteGuardrailConfig::default()
        };
        let engine = GuardrailsEngine::new(config);
        let mut req = request();
        req.schema_hint = None;
        let decision = engine
            .evaluate(&req, &chat_response(json!("not-json")))
            .unwrap();
        assert!(matches!(decision, GuardrailDecision::Reject { .. }));
    }

    #[test]
    fn evaluate_sanitizes_reject_error() {
        let mut config = GuardrailsConfig::default();
        config.chat_completions = RouteGuardrailConfig {
            mode: GuardrailMode::Enforce,
            validate_json_mode: true,
            ..RouteGuardrailConfig::default()
        };
        let engine = GuardrailsEngine::new(config);
        let mut req = request();
        req.schema_hint = None;
        let decision = engine
            .evaluate(&req, &chat_response(json!("not-json")))
            .unwrap();
        assert_eq!(
            decision,
            GuardrailDecision::Reject {
                client_error: "response failed guardrail validation".into()
            }
        );
    }

    #[test]
    fn recovery_retry_same_provider_returns_nudge() {
        let mut config = GuardrailsConfig::default();
        config.chat_completions = RouteGuardrailConfig {
            mode: GuardrailMode::Enforce,
            validate_json_mode: true,
            retry_with_nudge: false,
            ..RouteGuardrailConfig::default()
        };
        config.max_guardrail_retries = 1;
        config.recovery = RecoveryConfig {
            enabled: true,
            retry_same_provider: true,
            retry_fallback_provider: false,
            retry_model_swap: false,
        };
        let engine = GuardrailsEngine::new(config);
        let mut req = request();
        req.schema_hint = None;
        let decision = engine
            .evaluate(&req, &chat_response(json!("not-json")))
            .unwrap();
        assert!(matches!(decision, GuardrailDecision::RetryWithNudge { .. }));
    }

    fn chat_response(content: serde_json::Value) -> serde_json::Value {
        json!({
            "id": "cmpl_1",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    }
}
