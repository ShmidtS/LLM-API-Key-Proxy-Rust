use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use models::common::{ApiError, ErrorResponse};

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized,
    NotFound,
    Internal(String),
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
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".into())
            }
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
