use thiserror::Error;

#[derive(Debug, Error)]
pub enum GuardrailError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("rescue failed: {0}")]
    Rescue(String),
    #[error("nudge failed: {0}")]
    Nudge(String),
    #[error("recovery failed: {0}")]
    Recovery(String),
    #[error("compaction failed: {0}")]
    Compaction(String),
    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("maximum guardrail retries exceeded: {attempts}")]
    MaxRetriesExceeded { attempts: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_error_messages() {
        let err = GuardrailError::Validation("bad tool call".into());
        assert_eq!(err.to_string(), "validation failed: bad tool call");
    }
}
