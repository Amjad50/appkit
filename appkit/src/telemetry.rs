//! Logging via `tracing`. `RUST_LOG` overrides the configured filter.
//! With the `otel` feature and `OTEL_EXPORTER_OTLP_ENDPOINT` set, spans and metrics are
//! exported over OTLP as well; otherwise nothing extra runs.

use serde::{Deserialize, Serialize};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Pretty,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `pretty` for terminals, `json` for log collectors.
    pub format: Format,
    /// Default filter when `RUST_LOG` is unset, e.g. `info,wardtrace=debug`.
    pub filter: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { format: Format::Pretty, filter: "info".into() }
    }
}

/// Keeps exporters alive; flushes them when dropped.
pub struct Guard {
    #[cfg(feature = "otel")]
    otel: Option<crate::otel::Providers>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(feature = "otel")]
        if let Some(p) = self.otel.take() {
            p.shutdown();
        }
    }
}

pub fn init(service: &str, config: &Config) -> Guard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.filter));
    let fmt = match config.format {
        Format::Pretty => tracing_subscriber::fmt::layer().compact().boxed(),
        Format::Json => tracing_subscriber::fmt::layer().json().flatten_event(true).boxed(),
    };
    let registry = tracing_subscriber::registry().with(filter).with(fmt);
    #[cfg(feature = "otel")]
    {
        let otel = crate::otel::Providers::from_env(service);
        let layer = otel.as_ref().map(|p| p.tracing_layer());
        registry.with(layer).try_init().ok();
        return Guard { otel };
    }
    #[cfg(not(feature = "otel"))]
    {
        let _ = service;
        registry.try_init().ok();
        Guard {}
    }
}
