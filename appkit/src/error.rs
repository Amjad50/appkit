use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// Error body returned by every route: `{ "message": "..." }` with a matching status code.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    #[serde(skip)]
    #[schema(ignore)]
    pub status: StatusCode,
    /// Human-readable explanation, safe to show to users.
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    /// Logs the cause and returns a generic 500 so internals never reach clients.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(%cause, "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. Please try again.")
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}

/// Malformed JSON, query strings and path parameters become `{ message }` 400s too.
macro_rules! rejection {
    ($($ty:ty),*) => {$(
        impl From<$ty> for ApiError {
            fn from(rejection: $ty) -> Self {
                Self::new(rejection.status(), rejection.body_text())
            }
        }
    )*};
}
rejection!(
    axum::extract::rejection::JsonRejection,
    axum::extract::rejection::QueryRejection,
    axum::extract::rejection::PathRejection,
    axum::extract::multipart::MultipartRejection,
    axum::extract::multipart::MultipartError
);
