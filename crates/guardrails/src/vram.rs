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

pub trait GpuProbe: Send + Sync {
    fn probe(&self) -> Option<VramSnapshot>;
}

#[derive(Debug, Clone, Default)]
pub struct FakeGpuProbe {
    total_bytes: u64,
    free_bytes: u64,
    used_bytes: u64,
    device_count: u32,
}

impl FakeGpuProbe {
    pub fn new(total_bytes: u64, free_bytes: u64, used_bytes: u64, device_count: u32) -> Self {
        Self {
            total_bytes,
            free_bytes,
            used_bytes,
            device_count,
        }
    }
}

impl GpuProbe for FakeGpuProbe {
    fn probe(&self) -> Option<VramSnapshot> {
        Some(VramSnapshot {
            total_bytes: self.total_bytes,
            free_bytes: self.free_bytes,
            used_bytes: self.used_bytes,
            device_count: self.device_count,
            source: VramSource::StaticConfig,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct NvidiaSmiProbe;

impl GpuProbe for NvidiaSmiProbe {
    fn probe(&self) -> Option<VramSnapshot> {
        match std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,memory.total",
                "--format=csv,noheader,nounits",
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let line = stdout.lines().next()?;
                let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                if parts.len() < 2 {
                    return None;
                }
                let total_mb = parts[1].parse::<u64>().ok()?;
                Some(VramSnapshot {
                    total_bytes: total_mb * 1024 * 1024,
                    free_bytes: 0,
                    used_bytes: 0,
                    device_count: 1,
                    source: VramSource::NvidiaSmi,
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AmdSysfsProbe;

impl GpuProbe for AmdSysfsProbe {
    fn probe(&self) -> Option<VramSnapshot> {
        #[cfg(target_os = "linux")]
        {
            use std::path::Path;
            let drm_root = Path::new("/sys/class/drm");
            if !drm_root.exists() {
                return None;
            }
            let mut total_bytes = 0u64;
            let mut found = false;
            let entries = std::fs::read_dir(drm_root).ok()?;
            for entry in entries {
                let entry = entry.ok()?;
                let name = entry.file_name();
                let name_str = name.to_str()?;
                if !name_str.starts_with("card") {
                    continue;
                }
                if name_str.contains("render") {
                    continue;
                }
                let vram_file = entry.path().join("device/mem_info_vram_total");
                if let Ok(content) = std::fs::read_to_string(&vram_file)
                    && let Ok(bytes) = content.trim().parse::<u64>()
                {
                    total_bytes += bytes;
                    found = true;
                }
            }
            if found {
                Some(VramSnapshot {
                    total_bytes,
                    free_bytes: 0,
                    used_bytes: 0,
                    device_count: 1,
                    source: VramSource::AmdSysfs,
                })
            } else {
                None
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
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
    AmdSysfs,
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
            model
                .max_context_tokens
                .min((vram.free_bytes / 1024 / 1024) as usize)
        };
        Ok(ContextBudget {
            max_context_tokens,
            ..ContextBudget::default()
        })
    }
}
