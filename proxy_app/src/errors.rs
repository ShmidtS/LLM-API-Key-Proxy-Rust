use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use models::common::{ApiError, ErrorResponse};

pub fn invalid_request_error(msg: impl Into<String>) -> impl IntoResponse {
    error_response(
        StatusCode::BAD_REQUEST,
        msg,
        "invalid_request_error",
        StatusCode::BAD_REQUEST.as_u16().to_string(),
    )
}

pub fn payload_too_large_error() -> impl IntoResponse {
    error_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        "Request body too large",
        "invalid_request_error",
        StatusCode::PAYLOAD_TOO_LARGE.as_u16().to_string(),
    )
}

pub fn authentication_error() -> impl IntoResponse {
    error_response(
        StatusCode::UNAUTHORIZED,
        "Unauthorized",
        "authentication_error",
        StatusCode::UNAUTHORIZED.as_u16().to_string(),
    )
}

pub fn not_found_error(resource: impl Into<String>) -> impl IntoResponse {
    error_response(
        StatusCode::NOT_FOUND,
        format!("{} not found", resource.into()),
        "not_found_error",
        StatusCode::NOT_FOUND.as_u16().to_string(),
    )
}

pub fn rate_limit_error(retry_after: Option<u32>) -> impl IntoResponse {
    let message = match retry_after {
        Some(seconds) => format!("Rate limit exceeded. Retry after {seconds} seconds"),
        None => "Rate limit exceeded".into(),
    };

    error_response(
        StatusCode::TOO_MANY_REQUESTS,
        message,
        "rate_limit_error",
        "rate_limit_exceeded",
    )
}

pub fn api_error(msg: impl Into<String>) -> impl IntoResponse {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        msg,
        "api_error",
        StatusCode::INTERNAL_SERVER_ERROR.as_u16().to_string(),
    )
}

fn error_response(
    status: StatusCode,
    message: impl Into<String>,
    error_type: impl Into<String>,
    code: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: ApiError {
                message: message.into(),
                r#type: Some(error_type.into()),
                param: None,
                code: Some(code.into()),
            },
        }),
    )
}

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized,
    NotFound,
    Internal(String),
    UpstreamTimeout(String),
    Rotator(rotator::RotatorError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized".into()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found".into()),
            AppError::Internal(msg) => {
                tracing::error!("internal error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".into(),
                )
            }
            AppError::UpstreamTimeout(msg) => (StatusCode::GATEWAY_TIMEOUT, msg),
            AppError::Rotator(e) => {
                tracing::error!("rotator error: {}", e);
                (StatusCode::BAD_GATEWAY, e.to_string())
            }
        };
        let body = Json(ErrorResponse {
            error: ApiError {
                message,
                r#type: Some("api_error".into()),
                param: None,
                code: Some(status.as_u16().to_string()),
            },
        });
        (status, body).into_response()
    }
}

impl From<rotator::RotatorError> for AppError {
    fn from(e: rotator::RotatorError) -> Self {
        AppError::Rotator(e)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::BadRequest(e.to_string())
    }
}

impl From<guardrails::GuardrailError> for AppError {
    fn from(e: guardrails::GuardrailError) -> Self {
        AppError::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    async fn response_json(response: impl IntoResponse) -> (StatusCode, Value) {
        let response = response.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap();

        (status, body)
    }

    #[tokio::test]
    async fn invalid_request_error_returns_openai_error_shape() {
        let (status, body) = response_json(invalid_request_error("missing model")).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "missing model");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], Value::Null);
        assert_eq!(body["error"]["code"], "400");
    }

    #[tokio::test]
    async fn authentication_error_returns_openai_error_shape() {
        let (status, body) = response_json(authentication_error()).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["message"], "Unauthorized");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["param"], Value::Null);
        assert_eq!(body["error"]["code"], "401");
    }

    #[tokio::test]
    async fn not_found_error_returns_openai_error_shape() {
        let (status, body) = response_json(not_found_error("model")).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "model not found");
        assert_eq!(body["error"]["type"], "not_found_error");
        assert_eq!(body["error"]["param"], Value::Null);
        assert_eq!(body["error"]["code"], "404");
    }

    #[tokio::test]
    async fn rate_limit_error_returns_openai_error_shape() {
        let (status, body) = response_json(rate_limit_error(Some(30))).await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            body["error"]["message"],
            "Rate limit exceeded. Retry after 30 seconds"
        );
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["param"], Value::Null);
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    }

    #[tokio::test]
    async fn api_error_returns_openai_error_shape() {
        let (status, body) = response_json(api_error("upstream failed")).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "upstream failed");
        assert_eq!(body["error"]["type"], "api_error");
        assert_eq!(body["error"]["param"], Value::Null);
        assert_eq!(body["error"]["code"], "500");
    }
}
