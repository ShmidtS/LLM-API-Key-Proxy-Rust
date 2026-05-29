use crate::error::GuardrailError;
use crate::types::{GuardrailTrace, ValidationIssue};
use serde_json::Value;

pub trait StreamValidator: Send + Sync {
    fn validate_frame(
        &self,
        frame: &Value,
        trace: &mut GuardrailTrace,
    ) -> Result<(), GuardrailError>;
    fn finish(&self, trace: &mut GuardrailTrace) -> Result<(), GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct NoOpStreamValidator;

impl StreamValidator for NoOpStreamValidator {
    fn validate_frame(
        &self,
        _frame: &Value,
        _trace: &mut GuardrailTrace,
    ) -> Result<(), GuardrailError> {
        Ok(())
    }

    fn finish(&self, _trace: &mut GuardrailTrace) -> Result<(), GuardrailError> {
        Ok(())
    }
}

#[allow(dead_code)]
fn frame_issue(field: impl Into<String>, reason: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        field: field.into(),
        reason: reason.into(),
        severity: "warning".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn noop_accepts_any_frame() {
        let mut trace = GuardrailTrace::default();
        NoOpStreamValidator
            .validate_frame(&json!({"event":"delta"}), &mut trace)
            .unwrap();
        NoOpStreamValidator.finish(&mut trace).unwrap();
        assert!(trace.issues.is_empty());
    }
}
