//! OTLP/gRPC export of spans and metrics, enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//! Must be initialised inside the tokio runtime (the gRPC channel spawns on it).

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{SdkTracerProvider, Tracer};

pub(crate) struct Providers {
    traces: SdkTracerProvider,
    metrics: SdkMeterProvider,
    tracer: Tracer,
}

impl Providers {
    pub(crate) fn from_env(service: &str) -> Option<Self> {
        std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT")?;
        let resource = Resource::builder().with_service_name(service.to_string()).build();
        let spans = opentelemetry_otlp::SpanExporter::builder().with_tonic().build();
        let metrics = opentelemetry_otlp::MetricExporter::builder().with_tonic().build();
        let (spans, metrics) = match (spans, metrics) {
            (Ok(s), Ok(m)) => (s, m),
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("OpenTelemetry export disabled: {e}");
                return None;
            }
        };
        let traces = SdkTracerProvider::builder().with_batch_exporter(spans).with_resource(resource.clone()).build();
        let metrics = SdkMeterProvider::builder().with_periodic_exporter(metrics).with_resource(resource).build();
        opentelemetry::global::set_tracer_provider(traces.clone());
        opentelemetry::global::set_meter_provider(metrics.clone());
        let tracer = traces.tracer(service.to_string());
        Some(Self { traces, metrics, tracer })
    }

    pub(crate) fn tracing_layer<S>(&self) -> tracing_opentelemetry::OpenTelemetryLayer<S, Tracer>
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        tracing_opentelemetry::layer().with_tracer(self.tracer.clone())
    }

    pub(crate) fn shutdown(self) {
        self.traces.shutdown().ok();
        self.metrics.shutdown().ok();
    }
}
