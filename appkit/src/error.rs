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

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}

/// `?` works on any error. Extractor rejections keep their status and message; everything
/// else (database, IO, ...) is logged and becomes a generic 500.
/// `ApiError` itself is deliberately not a `std::error::Error`, so this impl is allowed.
impl<E: std::error::Error + 'static> From<E> for ApiError {
    fn from(error: E) -> Self {
        rejection(&error).unwrap_or_else(|| Self::internal(error))
    }
}

/// Malformed JSON, forms, query strings, path parameters and multipart bodies are the client's fault.
fn rejection(error: &dyn std::any::Any) -> Option<ApiError> {
    macro_rules! rejection {
        ($($ty:ty),*) => {$(
            if let Some(rejection) = error.downcast_ref::<$ty>() {
                return Some(ApiError::new(rejection.status(), rejection.body_text()));
            }
        )*};
    }
    rejection!(
        axum::extract::rejection::JsonRejection,
        axum::extract::rejection::FormRejection,
        axum::extract::rejection::QueryRejection,
        axum::extract::rejection::PathRejection,
        axum::extract::multipart::MultipartRejection,
        axum::extract::multipart::MultipartError
    );
    None
}

#[cfg(test)]
mod tests {
    use super::ApiError;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn question_mark_keeps_rejections_and_hides_other_errors() {
        use axum::extract::FromRequest;
        let request = axum::http::Request::new(axum::body::Body::empty());
        let rejection = axum::Json::<serde_json::Value>::from_request(request, &()).await.unwrap_err();
        assert_eq!(ApiError::from(rejection).status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let io = ApiError::from(std::io::Error::other("disk on fire"));
        assert_eq!(io.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!io.message.contains("disk"));
    }
}
