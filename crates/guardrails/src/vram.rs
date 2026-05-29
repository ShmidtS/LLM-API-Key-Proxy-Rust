use crate::error::GuardrailError;
use crate::types::{ContextBudget, GuardrailRequest};
use serde::{Deserialize, Serialize};

pub trait VramMonitor: Send + Sync {
    fn snapshot(&self) -> Result<VramSnapshot, GuardrailError>;
}

pub trait ModelMemoryEstimator: Send + Sync {
    fn estimate(
        &self,
        model: &str,
        requested_context_tokens: usize,
        output_tokens: usize,
    ) -> Result<ModelMemoryEstimate, GuardrailError>;
}

pub trait VramAwareContextManager: Send + Sync {
    fn budget_for(
        &self,
        request: &GuardrailRequest,
        model: &ModelDescriptor,
        vram: &VramSnapshot,
    ) -> Result<ContextBudget, GuardrailError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VramSnapshot {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub device_count: u32,
    pub source: VramSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VramSource {
    NvidiaSmi,
    RocmSmi,
    StaticConfig,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub provider: String,
    pub model: String,
    pub max_context_tokens: usize,
    pub quantization: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMemoryEstimate {
    pub weights_bytes: u64,
    pub kv_cache_bytes_per_token: u64,
    pub activation_headroom_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VramAction {
    Keep,
    Compact { target_context_tokens: usize },
    ReduceOutput { max_output_tokens: usize },
    Reject { reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct StaticVramContextManager;

impl VramAwareContextManager for StaticVramContextManager {
    fn budget_for(
        &self,
        _request: &GuardrailRequest,
        model: &ModelDescriptor,
        vram: &VramSnapshot,
    ) -> Result<ContextBudget, GuardrailError> {
        let max_context_tokens = if matches!(vram.source, VramSource::Unknown) {
            model.max_context_tokens
        } else {
            model.max_context_tokens.min((vram.free_bytes / 1024 / 1024) as usize)
        };
        Ok(ContextBudget {
            max_context_tokens,
            ..ContextBudget::default()
        })
    }
}
