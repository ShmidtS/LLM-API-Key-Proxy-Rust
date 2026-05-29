use async_trait::async_trait;

use crate::context::GuardrailContext;
use crate::error::GuardrailError;
use crate::types::{
    GuardrailDecision, GuardrailRequest, GuardrailResponse, GuardrailTrace, UpstreamErrorSummary,
};

#[async_trait]
pub trait GuardrailPipeline: Send + Sync {
    async fn before_request(
        &self,
        ctx: &mut GuardrailContext,
        request: GuardrailRequest,
    ) -> Result<GuardrailRequest, GuardrailError>;

    async fn after_response(
        &self,
        ctx: &mut GuardrailContext,
        response: GuardrailResponse,
    ) -> Result<GuardrailDecision, GuardrailError>;

    async fn on_upstream_error(
        &self,
        ctx: &mut GuardrailContext,
        error: UpstreamErrorSummary,
    ) -> Result<GuardrailDecision, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct NoopGuardrailPipeline;

#[async_trait]
impl GuardrailPipeline for NoopGuardrailPipeline {
    async fn before_request(
        &self,
        _ctx: &mut GuardrailContext,
        request: GuardrailRequest,
    ) -> Result<GuardrailRequest, GuardrailError> {
        Ok(request)
    }

    async fn after_response(
        &self,
        ctx: &mut GuardrailContext,
        response: GuardrailResponse,
    ) -> Result<GuardrailDecision, GuardrailError> {
        Ok(GuardrailDecision::Accept {
            response,
            trace: ctx.trace.clone(),
        })
    }

    async fn on_upstream_error(
        &self,
        ctx: &mut GuardrailContext,
        error: UpstreamErrorSummary,
    ) -> Result<GuardrailDecision, GuardrailError> {
        Ok(GuardrailDecision::Abort {
            internal_error: error.error_kind,
            trace: GuardrailTrace {
                issues: ctx.trace.issues.clone(),
                warnings: ctx.trace.warnings.clone(),
                actions_taken: ctx.trace.actions_taken.clone(),
            },
        })
    }
}
