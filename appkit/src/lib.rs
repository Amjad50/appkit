//! A small layer over axum: auto-registered routes with OpenAPI, layered config,
//! telemetry, background tasks and graceful shutdown. Plain axum keeps working inside it.

pub mod config;
mod error;
mod server;
mod state;
#[cfg(feature = "otel")]
mod otel;
pub mod telemetry;

pub use appkit_macros::{delete, get, patch, post, put};
pub use error::{ApiError, ApiResult};
pub use server::{Server, openapi};
pub use state::State;
pub use tokio_util::sync::CancellationToken;

#[doc(hidden)]
pub use {inventory, utoipa_axum};

/// One route registered by `#[get]`/`#[post]`/...; collected with `inventory`.
#[doc(hidden)]
pub struct Route {
    pub register: fn(utoipa_axum::router::OpenApiRouter) -> utoipa_axum::router::OpenApiRouter,
}
inventory::collect!(Route);

/// Metrics API (no-op unless the `otel` feature exports them).
#[cfg(feature = "otel")]
pub use opentelemetry::global::meter;
