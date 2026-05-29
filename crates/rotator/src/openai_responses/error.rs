use thiserror::Error;

pub type Result<T> = std::result::Result<T, ResponsesBridgeError>;

#[derive(Debug, Error)]
pub enum ResponsesBridgeError {
    #[error("unsupported tool type: {tool_type}")]
    UnsupportedToolType { tool_type: String },
    #[error("function tool missing function definition")]
    MissingFunctionDefinition,
    #[error("unsupported input part type: {part_type}")]
    UnsupportedInputPart { part_type: String },
    #[error("invalid tool choice: {reason}")]
    InvalidToolChoice { reason: String },
    #[error("serialization failed: {0}")]
    Serialization(String),
}

impl From<serde_json::Error> for ResponsesBridgeError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}
