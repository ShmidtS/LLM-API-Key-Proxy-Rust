use crate::error::GuardrailError;
use crate::types::{GuardrailRequest, GuardrailResponse, StepPolicy, ValidationIssue};

pub trait StepEnforcer: Send + Sync {
    fn before_request(
        &self,
        request: GuardrailRequest,
        policy: &StepPolicy,
    ) -> Result<GuardrailRequest, GuardrailError>;

    fn after_response(
        &self,
        response: &GuardrailResponse,
        policy: &StepPolicy,
    ) -> Result<Vec<ValidationIssue>, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultStepEnforcer;

impl StepEnforcer for DefaultStepEnforcer {
    fn before_request(
        &self,
        mut request: GuardrailRequest,
        policy: &StepPolicy,
    ) -> Result<GuardrailRequest, GuardrailError> {
        if request.step_policy.is_none() {
            request.step_policy = Some(policy.clone());
        }
        Ok(request)
    }

    fn after_response(
        &self,
        response: &GuardrailResponse,
        policy: &StepPolicy,
    ) -> Result<Vec<ValidationIssue>, GuardrailError> {
        let text = response.body.to_string().to_lowercase();
        let issues = policy
            .required_steps
            .iter()
            .filter(|step| !text.contains(&step.to_lowercase()))
            .map(|step| ValidationIssue {
                field: "steps".to_owned(),
                reason: format!("required step `{step}` was not completed"),
                severity: "error".to_owned(),
            })
            .collect();
        Ok(issues)
    }
}
