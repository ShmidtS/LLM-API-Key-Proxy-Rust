use crate::compaction::DefaultContextCompactor;
use crate::config::GuardrailsConfig;
use crate::context::GuardrailContext;
use crate::error::GuardrailError;
use crate::nudge::{DefaultRetryNudger, RetryNudger};
use crate::pipeline::GuardrailPipeline;
use crate::recovery::{DefaultErrorRecovery, ErrorRecovery, RecoveryAction};
use crate::streaming::{BufferingStreamValidator, StreamValidator};
use crate::tool_rescue::{DefaultToolCallRescuer, ToolCallRescuer};
use crate::types::{
    CompactionResult, GuardrailDecision, GuardrailMode, GuardrailRequest, GuardrailResponse,
    GuardrailTrace, SchemaHint, UpstreamErrorSummary, ValidationOptions, ValidationReport,
};
use crate::validation::{DefaultResponseValidator, ResponseValidator, extract_allowed_tools};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{debug, warn};

pub struct GuardrailsEngine {
    config: GuardrailsConfig,
    validator: Box<dyn ResponseValidator>,
    rescuer: Box<dyn ToolCallRescuer>,
    nudger: Box<dyn RetryNudger>,
    recovery: Box<dyn ErrorRecovery>,
    compactor: Box<DefaultContextCompactor>,
    stream_validator: Box<dyn StreamValidator>,
    step_tracker: std::sync::Mutex<crate::prerequisites::StepTracker>,
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
            stream_validator: Box::new(BufferingStreamValidator::default()),
            step_tracker: std::sync::Mutex::new(crate::prerequisites::StepTracker::new()),
        }
    }

    pub fn with_components(
        config: GuardrailsConfig,
        validator: Box<dyn ResponseValidator>,
        rescuer: Box<dyn ToolCallRescuer>,
        nudger: Box<dyn RetryNudger>,
        recovery: Box<dyn ErrorRecovery>,
        compactor: Box<DefaultContextCompactor>,
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
            step_tracker: std::sync::Mutex::new(crate::prerequisites::StepTracker::new()),
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
        let normalized = GuardrailResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: response.clone(),
        };
        self.evaluate_response(request, normalized)
    }

    pub fn evaluate_response(
        &self,
        request: &GuardrailRequest,
        mut response: GuardrailResponse,
    ) -> Result<GuardrailDecision, GuardrailError> {
        let route_config = self.config.route_config(&request.route);
        let trace = GuardrailTrace::default();
        // Синтетический respond-tool превращается в обычный текстовый ответ до любой
        // валидации, чтобы он не утёк клиенту и не считался невалидным tool_call.
        // Чистый respond-ответ — это conversational-ответ Forge: он принимается как
        // финальный текст и не подчиняется JSON-mode/schema валидации.
        if crate::respond::strip_respond_tool_calls(&mut response.body) {
            return Ok(GuardrailDecision::Accept { response, trace });
        }
        if route_config.mode == GuardrailMode::Off {
            return Ok(GuardrailDecision::Accept { response, trace });
        }

        // Terminal tool enforcement: блокировать вызов terminal-tool до выполнения
        // required_steps (паритет с Forge StepEnforcer::check).
        if let Some(policy) = &request.step_policy {
            let mut tracker = self.step_tracker.lock().unwrap();
            tracker.set_required_steps(policy.required_steps.clone());
            let tool_names = crate::steps::extract_tool_names_from_response(&response);
            for name in &tool_names {
                tracker.record(name, Value::Null);
            }
            if let Some(msg) = crate::steps::check_premature_terminal(&response, policy, &tracker)
                && route_config.mode == GuardrailMode::Enforce
            {
                let mut trace = GuardrailTrace::default();
                trace
                    .actions_taken
                    .push("step_enforcement_error".to_owned());
                return Ok(GuardrailDecision::Retry {
                    request: request.clone(),
                    reason: msg,
                    trace,
                });
            }
        }

        let options = ValidationOptions {
            validate_tool_calls: route_config.validate_tool_calls,
            validate_json_mode: route_config.validate_json_mode,
            validate_schema: route_config.validate_schema,
            validate_steps: route_config.validate_steps,
            allowed_tools: extract_allowed_tools(&request.body),
        };
        let report = self.validator.validate(request, &response, &options)?;
        if report.ok || route_config.mode == GuardrailMode::Observe {
            return Ok(GuardrailDecision::Accept { response, trace });
        }

        warn!(
            violations = report.violations.len(),
            details = ?report.violations,
            "guardrail validation failed"
        );

        if route_config.rescue_tool_calls
            && has_tool_call_violation(&report)
            && let Some(candidate) = self.rescuer.rescue(&response)?
        {
            if candidate.remaining_issues.is_empty() && !candidate.repaired_fields.is_empty() {
                debug!(fields = ?candidate.repaired_fields, "guardrail rescued response");
                return Ok(GuardrailDecision::Repair {
                    response: candidate.response,
                    repaired_fields: candidate.repaired_fields,
                    trace,
                });
            }
            debug!(issues = ?candidate.remaining_issues, "guardrail tool rescue skipped");
        }

        if route_config.retry_with_nudge
            && self.config.max_guardrail_retries > 0
            && request.attempt.semantic_retry_index < self.config.max_guardrail_retries
        {
            let request = self.nudger.nudge(request, &report)?;
            return Ok(GuardrailDecision::Retry {
                request,
                reason: retry_reason(&report),
                trace,
            });
        }

        let action = self
            .recovery
            .recover_validation(request, &report, &self.config)?;
        debug!(?action, "guardrail recovery decision");

        match action {
            RecoveryAction::RetrySameRequest => Ok(GuardrailDecision::Retry {
                request: request.clone(),
                reason: retry_reason(&report),
                trace,
            }),
            RecoveryAction::RetryWithNudge => Ok(GuardrailDecision::Retry {
                request: self.nudger.nudge(request, &report)?,
                reason: retry_reason(&report),
                trace,
            }),
            RecoveryAction::RetryAfterCompaction => Ok(GuardrailDecision::Retry {
                request: self.preprocess(request)?,
                reason: retry_reason(&report),
                trace,
            }),
            RecoveryAction::UseRepairedResponse | RecoveryAction::GiveUp => {
                Ok(GuardrailDecision::Reject {
                    client_error: sanitized_client_error(),
                    trace,
                })
            }
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
            .compact_with_config(&processed, &self.config.context_compaction)?
        {
            CompactionResult::Unchanged => {}
            CompactionResult::Compacted { body, .. } => {
                processed.body = body;
            }
        }

        crate::respond::inject_respond_tool(Arc::make_mut(&mut processed.body));

        if route_config.validate_steps {
            apply_step_enforcement(&mut processed);
        }

        let mut trace = GuardrailTrace::default();
        let _ = self.stream_validator.finish(&mut trace)?;

        Ok(processed)
    }

    pub fn evaluate_stream(
        &self,
        request: &GuardrailRequest,
        frames: Vec<Value>,
    ) -> Result<GuardrailDecision, GuardrailError> {
        let mut trace = GuardrailTrace::default();
        let validator = BufferingStreamValidator::default();
        for frame in &frames {
            validator.validate_frame(frame, &mut trace)?;
        }
        let body = validator.finish(&mut trace)?;
        let response = GuardrailResponse {
            status: 200,
            headers: BTreeMap::new(),
            body,
        };
        self.evaluate_response(request, response)
    }
}

#[async_trait]
impl GuardrailPipeline for GuardrailsEngine {
    async fn before_request(
        &self,
        ctx: &mut GuardrailContext,
        request: GuardrailRequest,
    ) -> Result<GuardrailRequest, GuardrailError> {
        if let Some(policy) = &request.step_policy {
            ctx.step_policy = Some(policy.clone());
        }
        ctx.attempt = request.attempt.clone();
        ctx.request = Some(request.clone());
        self.preprocess(&request)
    }

    async fn after_response(
        &self,
        ctx: &mut GuardrailContext,
        response: GuardrailResponse,
    ) -> Result<GuardrailDecision, GuardrailError> {
        let request = ctx.request.clone().unwrap_or_else(|| GuardrailRequest {
            route: ctx.route.clone(),
            provider: String::new(),
            upstream_path: String::new(),
            model: String::new(),
            body: Arc::new(Value::Null),
            stream: false,
            schema_hint: None,
            step_policy: ctx.step_policy.clone(),
            attempt: ctx.attempt.clone(),
        });
        self.evaluate_response(&request, response)
    }

    async fn on_upstream_error(
        &self,
        ctx: &mut GuardrailContext,
        error: UpstreamErrorSummary,
    ) -> Result<GuardrailDecision, GuardrailError> {
        Ok(GuardrailDecision::Abort {
            internal_error: error.error_kind,
            trace: ctx.trace.clone(),
        })
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

    if let Some(messages) = Arc::make_mut(&mut request.body)
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
        Arc::make_mut(&mut request.body)["guardrail_step_policy"] = serde_json::json!({
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
