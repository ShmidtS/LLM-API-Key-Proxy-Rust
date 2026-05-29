use crate::config::GuardrailsConfig;
use crate::error::GuardrailError;
use crate::types::{UpstreamErrorSummary, ValidationReport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    RetrySameProvider,
    RetryFallbackProvider,
    RetryModelSwap,
    GiveUp,
}

pub trait ErrorRecovery: Send + Sync {
    fn recover_upstream(
        &self,
        error: &UpstreamErrorSummary,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError>;

    fn recover_validation(
        &self,
        report: &ValidationReport,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultErrorRecovery;

impl ErrorRecovery for DefaultErrorRecovery {
    fn recover_upstream(
        &self,
        error: &UpstreamErrorSummary,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError> {
        if !config.recovery.enabled {
            return Ok(RecoveryAction::GiveUp);
        }

        if matches!(error.status_code, Some(429 | 500 | 502 | 503 | 504))
            && config.recovery.retry_fallback_provider
        {
            return Ok(RecoveryAction::RetryFallbackProvider);
        }

        if matches!(error.status_code, Some(408 | 409 | 425 | 429))
            && config.recovery.retry_same_provider
        {
            return Ok(RecoveryAction::RetrySameProvider);
        }

        if error.error_kind == "model_unavailable" && config.recovery.retry_model_swap {
            return Ok(RecoveryAction::RetryModelSwap);
        }

        Ok(RecoveryAction::GiveUp)
    }

    fn recover_validation(
        &self,
        report: &ValidationReport,
        config: &GuardrailsConfig,
    ) -> Result<RecoveryAction, GuardrailError> {
        if !config.recovery.enabled || report.ok {
            return Ok(RecoveryAction::GiveUp);
        }

        if report
            .violations
            .iter()
            .any(|issue| issue.field.contains("tool_calls") || issue.reason.contains("JSON"))
            && config.recovery.retry_same_provider
        {
            return Ok(RecoveryAction::RetrySameProvider);
        }

        Ok(RecoveryAction::GiveUp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RecoveryConfig;
    use crate::types::ValidationIssue;

    #[test]
    fn disabled_recovery_gives_up() {
        let action = DefaultErrorRecovery
            .recover_upstream(
                &UpstreamErrorSummary {
                    status_code: Some(503),
                    provider_error_message: None,
                    error_kind: "server_error".into(),
                },
                &GuardrailsConfig::default(),
            )
            .unwrap();
        assert_eq!(action, RecoveryAction::GiveUp);
    }

    #[test]
    fn retry_fallback_for_transient_upstream_error() {
        let mut config = GuardrailsConfig::default();
        config.recovery = RecoveryConfig {
            enabled: true,
            retry_same_provider: false,
            retry_fallback_provider: true,
            retry_model_swap: false,
        };
        let action = DefaultErrorRecovery
            .recover_upstream(
                &UpstreamErrorSummary {
                    status_code: Some(503),
                    provider_error_message: Some("busy".into()),
                    error_kind: "server_error".into(),
                },
                &config,
            )
            .unwrap();
        assert_eq!(action, RecoveryAction::RetryFallbackProvider);
    }

    #[test]
    fn retry_same_provider_for_json_validation() {
        let mut config = GuardrailsConfig::default();
        config.recovery.enabled = true;
        config.recovery.retry_same_provider = true;
        let report = ValidationReport {
            ok: false,
            violations: vec![ValidationIssue {
                field: "response".into(),
                reason: "JSON invalid".into(),
                severity: "error".into(),
            }],
        };
        let action = DefaultErrorRecovery
            .recover_validation(&report, &config)
            .unwrap();
        assert_eq!(action, RecoveryAction::RetrySameProvider);
    }
}
