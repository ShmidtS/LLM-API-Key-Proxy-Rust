use crate::context::GuardrailContext;
use crate::error::GuardrailError;
use crate::pipeline::GuardrailPipeline;
use crate::types::{GuardrailDecision, GuardrailRequest, GuardrailResponse, UpstreamErrorSummary};

#[derive(Debug, Clone)]
pub struct GuardrailMiddleware<P> {
    pipeline: P,
}

impl<P> GuardrailMiddleware<P> {
    pub fn new(pipeline: P) -> Self {
        Self { pipeline }
    }

    pub fn pipeline(&self) -> &P {
        &self.pipeline
    }
}

impl<P: GuardrailPipeline> GuardrailMiddleware<P> {
    pub async fn before_request(
        &self,
        ctx: &mut GuardrailContext,
        request: GuardrailRequest,
    ) -> Result<GuardrailRequest, GuardrailError> {
        self.pipeline.before_request(ctx, request).await
    }

    pub async fn after_response(
        &self,
        ctx: &mut GuardrailContext,
        response: GuardrailResponse,
    ) -> Result<GuardrailDecision, GuardrailError> {
        self.pipeline.after_response(ctx, response).await
    }

    pub async fn on_error(
        &self,
        ctx: &mut GuardrailContext,
        error: UpstreamErrorSummary,
    ) -> Result<GuardrailDecision, GuardrailError> {
        self.pipeline.on_upstream_error(ctx, error).await
    }
}
