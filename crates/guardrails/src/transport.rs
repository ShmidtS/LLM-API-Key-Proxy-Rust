use async_trait::async_trait;
use thiserror::Error;

use crate::types::{GuardrailRequest, GuardrailResponse};

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("upstream transport failed: {0}")]
    Upstream(String),
    #[error("response serialization failed: {0}")]
    Serialization(String),
}

#[async_trait]
pub trait GuardedTransport: Send + Sync {
    async fn send(&self, request: GuardrailRequest) -> Result<GuardrailResponse, TransportError>;
}
