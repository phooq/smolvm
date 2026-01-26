//! API error types with HTTP status mapping.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// API error type with HTTP status code mapping.
#[derive(Debug)]
pub enum ApiError {
    /// Resource not found (404).
    NotFound(String),
    /// Conflict - resource already exists or invalid state (409).
    Conflict(String),
    /// Bad request - invalid input (400).
    BadRequest(String),
    /// Request timeout (408).
    Timeout,
    /// Internal server error (500).
    Internal(String),
}

/// JSON error response body.
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    code: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            ApiError::Timeout => (
                StatusCode::REQUEST_TIMEOUT,
                "TIMEOUT",
                "request timed out".to_string(),
            ),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", msg),
        };

        let body = Json(ErrorResponse {
            error: message,
            code,
        });

        (status, body).into_response()
    }
}

impl From<crate::error::Error> for ApiError {
    fn from(err: crate::error::Error) -> Self {
        match &err {
            crate::error::Error::VmNotFound(name) => {
                ApiError::NotFound(format!("sandbox not found: {}", name))
            }
            crate::error::Error::InvalidState { expected, actual } => ApiError::Conflict(format!(
                "invalid state: expected {}, got {}",
                expected, actual
            )),
            crate::error::Error::AgentError(msg) => {
                if msg.contains("not found") {
                    ApiError::NotFound(msg.clone())
                } else if msg.contains("already") {
                    ApiError::Conflict(msg.clone())
                } else {
                    ApiError::Internal(msg.clone())
                }
            }
            _ => ApiError::Internal(err.to_string()),
        }
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        ApiError::Internal(format!("task failed: {}", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn test_api_error_status_codes() {
        let cases = [
            (ApiError::NotFound("x".into()), StatusCode::NOT_FOUND),
            (ApiError::Conflict("x".into()), StatusCode::CONFLICT),
            (ApiError::BadRequest("x".into()), StatusCode::BAD_REQUEST),
            (ApiError::Timeout, StatusCode::REQUEST_TIMEOUT),
            (
                ApiError::Internal("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.into_response().status(), expected);
        }
    }

    #[test]
    fn test_agent_error_keyword_detection() {
        // "not found" in message -> NotFound
        let err = crate::error::Error::AgentError("container not found".into());
        assert!(matches!(ApiError::from(err), ApiError::NotFound(_)));

        // "already" in message -> Conflict
        let err = crate::error::Error::AgentError("already exists".into());
        assert!(matches!(ApiError::from(err), ApiError::Conflict(_)));

        // No keywords -> Internal
        let err = crate::error::Error::AgentError("connection refused".into());
        assert!(matches!(ApiError::from(err), ApiError::Internal(_)));
    }
}
