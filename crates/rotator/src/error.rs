use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum RotatorError {
    #[error("no credentials available for provider: {0}")]
    NoCredentials(String),
    #[error("circuit breaker open for provider: {0}")]
    CircuitBreakerOpen(String),
    #[error("rate limited for provider: {0}, retry after: {1:?}")]
    RateLimited(String, Option<u64>),
    #[error("http error: {0}")]
    Http(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("all providers exhausted after {0} retries")]
    Exhausted(usize),
    #[error("timeout")]
    Timeout,
    #[error("unknown error: {0}")]
    Other(String),
}

impl From<reqwest::Error> for RotatorError {
    fn from(e: reqwest::Error) -> Self {
        RotatorError::Http(e.to_string())
    }
}

impl From<serde_json::Error> for RotatorError {
    fn from(e: serde_json::Error) -> Self {
        RotatorError::Serialization(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, RotatorError>;
