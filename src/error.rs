use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::models::api::{ApiError, ApiResponse};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Mode not allowed: {0}")]
    ModeNotAllowed(String),

    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),

    #[error("File too large: {0}")]
    FileTooLarge(String),

    #[error("AI temporarily unavailable: {0}")]
    AiTemporarilyUnavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "INVALID_REQUEST",
            Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::Forbidden(_) => "FORBIDDEN",
            Self::NotFound(_) => "RESOURCE_NOT_FOUND",
            Self::Conflict(_) => "CONFLICT",
            Self::ModeNotAllowed(_) => "MODE_NOT_ALLOWED",
            Self::UnsupportedFileType(_) => "UNSUPPORTED_FILE_TYPE",
            Self::FileTooLarge(_) => "FILE_TOO_LARGE",
            Self::AiTemporarilyUnavailable(_) => "AI_TEMPORARILY_UNAVAILABLE",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::ModeNotAllowed(_) => StatusCode::BAD_REQUEST,
            Self::UnsupportedFileType(_) => StatusCode::BAD_REQUEST,
            Self::FileTooLarge(_) => StatusCode::BAD_REQUEST,
            Self::AiTemporarilyUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Internal(_) | Self::Conflict(_) | Self::AiTemporarilyUnavailable(_)
        )
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "The request is invalid. Check the fields and try again.",
            Self::Unauthorized(_) => "Authentication is required.",
            Self::Forbidden(_) => "Access denied.",
            Self::NotFound(_) => "Resource not found.",
            Self::Conflict(_) => "The request cannot be processed right now.",
            Self::ModeNotAllowed(_) => "Mode is not allowed.",
            Self::UnsupportedFileType(_) => "Unsupported file type.",
            Self::FileTooLarge(_) => "File is too large.",
            Self::AiTemporarilyUnavailable(_) => {
                "The AI service is temporarily unavailable. Please try again later."
            }
            Self::Internal(_) => "An error occurred. Please try again later.",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::BadRequest(msg)
            | Self::Unauthorized(msg)
            | Self::Forbidden(msg)
            | Self::NotFound(msg)
            | Self::Conflict(msg)
            | Self::ModeNotAllowed(msg)
            | Self::UnsupportedFileType(msg)
            | Self::FileTooLarge(msg)
            | Self::AiTemporarilyUnavailable(msg)
            | Self::Internal(msg) => msg,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code().to_string();
        let message = self.public_message().to_string();
        let internal_message = self.message().to_string();
        let retryable = self.retryable();

        let req_id = uuid::Uuid::new_v4().to_string();
        if status.is_server_error() {
            tracing::error!(
                response_request_id = %req_id,
                code = %code,
                status = status.as_u16(),
                error = %internal_message,
                "request failed"
            );
        } else {
            tracing::warn!(
                response_request_id = %req_id,
                code = %code,
                status = status.as_u16(),
                error = %internal_message,
                "request rejected"
            );
        }

        let body = ApiResponse::<serde_json::Value>::error(
            ApiError {
                code,
                message,
                details: None,
                retryable,
            },
            req_id,
        );

        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::AppError;

    #[test]
    fn internal_errors_have_generic_public_message() {
        let err = AppError::Internal("database password leaked in stack trace".to_string());
        assert_eq!(
            err.public_message(),
            "An error occurred. Please try again later."
        );
        assert!(err.message().contains("database password"));
    }

    #[test]
    fn validation_errors_do_not_expose_field_specific_details_publicly() {
        let err = AppError::BadRequest("stack_main is required".to_string());
        assert_eq!(
            err.public_message(),
            "The request is invalid. Check the fields and try again."
        );
        assert!(err.message().contains("stack_main"));
    }

    #[test]
    fn ai_temporarily_unavailable_has_generic_public_message() {
        let err = AppError::AiTemporarilyUnavailable("Gemini 503 then Claude timeout".to_string());
        assert_eq!(err.code(), "AI_TEMPORARILY_UNAVAILABLE");
        assert_eq!(
            err.public_message(),
            "The AI service is temporarily unavailable. Please try again later."
        );
        assert!(err.retryable());
    }
}
