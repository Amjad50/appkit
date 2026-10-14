use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::ApiError;

/// Application state given to `Server::state`. Same shape as axum's `State`, but read from
/// request extensions so auto-registered routes need no typed router.
pub struct State<T>(pub T);

impl<T, S> FromRequestParts<S> for State<T>
where
    T: Clone + Send + Sync + 'static,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<T>()
            .cloned()
            .map(State)
            .ok_or_else(|| ApiError::internal(format!("missing state {}", std::any::type_name::<T>())))
    }
}
