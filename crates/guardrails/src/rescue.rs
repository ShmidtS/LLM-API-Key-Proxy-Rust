use crate::error::GuardrailError;
use crate::types::{GuardrailRequest, GuardrailResponse, RescueCandidate, ValidationReport};

pub trait RescueParser: Send + Sync {
    fn rescue(
        &self,
        request: &GuardrailRequest,
        response: &GuardrailResponse,
        report: &ValidationReport,
    ) -> Result<Option<RescueCandidate>, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultRescueParser;

impl RescueParser for DefaultRescueParser {
    fn rescue(
        &self,
        _request: &GuardrailRequest,
        _response: &GuardrailResponse,
        _report: &ValidationReport,
    ) -> Result<Option<RescueCandidate>, GuardrailError> {
        Ok(None)
    }
}
