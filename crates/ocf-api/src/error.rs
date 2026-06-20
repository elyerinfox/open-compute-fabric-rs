//! Translation of the canonical [`ocf_core::error::Error`] into HTTP responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ocf_core::error::Error;
use serde::Serialize;

/// A thin wrapper so we can implement `IntoResponse` for the fabric error type
/// without orphan-rule trouble. Handlers return `Result<T, ApiError>`.
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::AlreadyExists(_) | Error::Conflict(_) => StatusCode::CONFLICT,
            Error::InvalidArgument(_) => StatusCode::BAD_REQUEST,
            Error::NotSupported(_) => StatusCode::NOT_IMPLEMENTED,
            Error::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::Provider { .. }
            | Error::Io(_)
            | Error::Serde(_)
            | Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = ErrorBody {
            code: self.0.code(),
            message: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

/// Convenience alias for handler return types.
pub type ApiResult<T> = std::result::Result<T, ApiError>;
