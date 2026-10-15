//! A small layer over axum: auto-registered routes with OpenAPI, layered config,
//! telemetry, background tasks and graceful shutdown. Plain axum keeps working inside it.

pub mod config;
mod error;
#[cfg(feature = "otel")]
mod otel;
mod security;
mod server;
mod state;
pub mod telemetry;

pub use appkit_macros::{delete, get, patch, post, put};
pub use error::{ApiError, ApiResult};
pub use security::Security;
pub use server::{Server, openapi};
pub use state::State;
pub use tokio_util::sync::CancellationToken;

#[doc(hidden)]
pub use {inventory, utoipa_axum};

#[doc(hidden)]
pub mod __private {
    pub use crate::security::{Found, Probe, Secured, Unsecured, register};
}

/// One route registered by `#[get]`/`#[post]`/...; collected with `inventory`.
#[doc(hidden)]
pub struct Route {
    pub register: fn(utoipa_axum::router::OpenApiRouter) -> utoipa_axum::router::OpenApiRouter,
}
inventory::collect!(Route);

/// Metrics API (no-op unless the `otel` feature exports them).
#[cfg(feature = "otel")]
pub use opentelemetry::global::meter;
