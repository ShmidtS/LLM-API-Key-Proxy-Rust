use crate::config::GuardrailsConfig;
use crate::error::GuardrailError;
use crate::types::{GuardrailRequest, UpstreamErrorSummary, ValidationReport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    RetrySameRequest,
    RetryWithNudge,
    RetryAfterCompaction,
    UseRepairedResponse,
    GiveUp,
}

pub trait ErrorRecovery: Send + Sync {
    fn recover_upstream(
        &self,
        request: &GuardrailRequest,
        error: &UpstreamErrorSummary,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError>;

    fn recover_validation(
        &self,
        request: &GuardrailRequest,
        report: &ValidationReport,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultErrorRecovery;

impl ErrorRecovery for DefaultErrorRecovery {
    fn recover_upstream(
        &self,
        _request: &GuardrailRequest,
        error: &UpstreamErrorSummary,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError> {
        if !config.recovery.enabled {
            return Ok(RecoveryAction::GiveUp);
        }

        if matches!(
            error.status_code,
            Some(408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
        ) && config.recovery.retry_same_provider
        {
            return Ok(RecoveryAction::RetrySameRequest);
        }

        Ok(RecoveryAction::GiveUp)
    }

    fn recover_validation(
        &self,
        request: &GuardrailRequest,
        report: &ValidationReport,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError> {
        if !config.recovery.enabled || report.ok {
            return Ok(RecoveryAction::GiveUp);
        }

        if request.attempt.semantic_retry_index >= request.attempt.max_semantic_retries {
            return Ok(RecoveryAction::GiveUp);
        }

        if report
            .violations
            .iter()
            .any(|issue| issue.field.contains("tool_calls"))
        {
            return Ok(RecoveryAction::UseRepairedResponse);
        }

        if report
            .violations
            .iter()
            .any(|issue| issue.reason.contains("JSON") || issue.reason.contains("schema"))
        {
            return Ok(RecoveryAction::RetryWithNudge);
        }

        Ok(RecoveryAction::GiveUp)
    }
}
