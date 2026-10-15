use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use utoipa::openapi::{Info, OpenApi, Paths};
use utoipa_axum::router::OpenApiRouter;

use crate::{ApiError, Route};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Task = Box<dyn FnOnce(CancellationToken) -> BoxFuture<Result<(), String>> + Send>;
type Check = Box<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>;

/// Builds the OpenAPI document from every `#[get]`/`#[post]`/... route in the binary.
pub fn openapi(title: &str, version: &str) -> serde_json::Value {
    let mut spec = serde_json::to_value(collect(title, version).1).expect("OpenAPI serializes");
    require_response_fields(&mut spec);
    spec
}

/// serde always writes every field of a response (as `null` when empty), but utoipa marks
/// `Option` fields optional. Mark all properties required in schemas that only appear in
/// responses, so generated clients see `T | null` instead of `T | null | undefined`.
/// Schemas also reachable from request bodies or parameters keep their optional fields.
fn require_response_fields(spec: &mut serde_json::Value) {
    use serde_json::Value;
    use std::collections::HashSet;

    fn refs(value: &Value, schemas: &Value, found: &mut HashSet<String>) {
        match value {
            Value::Object(map) => {
                if let Some(name) = map.get("$ref").and_then(Value::as_str).and_then(|r| r.rsplit('/').next()) {
                    if found.insert(name.to_string()) {
                        if let Some(schema) = schemas.get(name) {
                            refs(schema, schemas, found);
                        }
                    }
                }
                map.values().for_each(|v| refs(v, schemas, found));
            }
            Value::Array(items) => items.iter().for_each(|v| refs(v, schemas, found)),
            _ => {}
        }
    }

    fn require(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(props)) = map.get("properties") {
                    let names = props.keys().cloned().map(Value::String).collect();
                    map.insert("required".into(), Value::Array(names));
                }
                map.values_mut().for_each(require);
            }
            Value::Array(items) => items.iter_mut().for_each(require),
            _ => {}
        }
    }

    let schemas = spec.pointer("/components/schemas").cloned().unwrap_or_default();
    let mut request = HashSet::new();
    for item in spec["paths"].as_object_mut().into_iter().flat_map(|p| p.values_mut()) {
        for op in item.as_object_mut().into_iter().flat_map(|o| o.values_mut()) {
            for key in ["requestBody", "parameters"] {
                if let Some(value) = op.get(key) {
                    refs(value, &schemas, &mut request);
                }
            }
            if let Some(responses) = op.get_mut("responses") {
                require(responses);
            }
        }
    }
    if let Some(Value::Object(schemas)) = spec.pointer_mut("/components/schemas") {
        for (_, schema) in schemas.iter_mut().filter(|(name, _)| !request.contains(*name)) {
            require(schema);
        }
    }
}

fn collect(title: &str, version: &str) -> (Router, OpenApi) {
    let mut routes = OpenApiRouter::with_openapi(OpenApi::new(Info::new(title, version), Paths::new()));
    for route in inventory::iter::<Route> {
        routes = (route.register)(routes);
    }
    routes.split_for_parts()
}

const TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP server with auto-registered routes, OpenAPI, docs, health checks and background tasks.
pub struct Server {
    title: String,
    version: String,
    body_limit: usize,
    extra: Router,
    layers: Vec<Box<dyn FnOnce(Router) -> Router + Send>>,
    tasks: Vec<(String, Task)>,
    checks: Vec<(String, Check)>,
}

impl Server {
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            body_limit: 2 * 1024 * 1024,
            extra: Router::new(),
            layers: Vec::new(),
            tasks: Vec::new(),
            checks: Vec::new(),
        }
    }

    /// Makes `appkit::State<T>` available to every route.
    pub fn state<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
        self.layers.push(Box::new(move |r| r.layer(axum::Extension(value))));
        self
    }

    pub fn body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = bytes;
        self
    }

    /// Plain axum routes (static files, fallbacks) merged after the API routes.
    pub fn router(mut self, router: Router) -> Self {
        self.extra = self.extra.merge(router);
        self
    }

    /// A long-running task. It must return when the token is cancelled.
    /// An error from any task shuts the whole server down.
    pub fn task<F, Fut, E>(mut self, name: &str, f: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), E>> + Send + 'static,
        E: std::fmt::Display,
    {
        self.tasks.push((name.into(), Box::new(move |token| Box::pin(async move { f(token).await.map_err(|e| e.to_string()) }))));
        self
    }

    /// A readiness check for `/readyz`.
    pub fn ready<F, Fut>(mut self, name: &str, check: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.checks.push((name.into(), Box::new(move || Box::pin(check()))));
        self
    }

    /// The fully assembled router, without serving it (used by tests).
    pub fn into_router(self) -> Router {
        self.build().0
    }

    fn build(self) -> (Router, Vec<(String, Task)>) {
        let (api, openapi) = collect(&self.title, &self.version);
        let mut openapi = serde_json::to_value(openapi).expect("OpenAPI serializes");
        require_response_fields(&mut openapi);
        let spec = serde_json::to_string(&openapi).expect("OpenAPI serializes");
        let docs = Html(DOCS.replace("{title}", &self.title));
        let checks = std::sync::Arc::new(self.checks);
        let mut router = api
            .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store")))
            .layer(middleware::from_fn(same_origin))
            .route("/api/openapi.json", get(move || async move { ([(header::CONTENT_TYPE, "application/json")], spec) }))
            .route("/api/docs", get(move || async move { docs }))
            .route("/healthz", get(|| async { "ok" }))
            .route(
                "/readyz",
                get(move || {
                    let checks = checks.clone();
                    async move {
                        for (name, check) in checks.iter() {
                            if let Err(e) = check().await {
                                return ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("{name}: {e}")).into_response();
                            }
                        }
                        "ready".into_response()
                    }
                }),
            )
            .merge(self.extra);
        for layer in self.layers {
            router = layer(router);
        }
        let router = router
            .layer(DefaultBodyLimit::max(self.body_limit))
            .layer(CompressionLayer::new())
            .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, TIMEOUT))
            .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
                let route = req.extensions().get::<MatchedPath>().map(|p| p.as_str());
                let name = route.map_or_else(|| req.method().to_string(), |r| format!("{} {r}", req.method()));
                let route = route.unwrap_or("-");
                tracing::info_span!("http", otel.name = %name, method = %req.method(), route, path = %req.uri().path())
            }));
        (router, self.tasks)
    }

    /// Binds `addr` and serves until Ctrl-C/SIGTERM or until a task fails, then drains in-flight work.
    pub async fn serve(self, addr: SocketAddr) -> Result<(), String> {
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("{addr}: {e}"))?;
        tracing::info!(addr = %listener.local_addr().unwrap_or(addr), "{} listening", self.title);
        let (router, tasks) = self.build();
        let token = CancellationToken::new();
        let mut set = JoinSet::new();
        for (name, task) in tasks {
            let token = token.clone();
            set.spawn(async move {
                let result = task(token.clone()).await;
                if let Err(e) = &result {
                    tracing::error!(task = %name, error = %e, "task failed; shutting down");
                }
                token.cancel();
                result.map_err(|e| format!("{name}: {e}"))
            });
        }
        let stop = token.clone();
        let served = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move {
                tokio::select! { _ = shutdown_signal() => {}, _ = stop.cancelled() => {} }
                stop.cancel();
            })
            .await
            .map_err(|e| e.to_string());
        token.cancel();
        let mut failure = served.err();
        while let Ok(Some(joined)) = tokio::time::timeout(Duration::from_secs(10), set.join_next()).await {
            if let Ok(Err(e)) = joined {
                failure.get_or_insert(e);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}

/// Unsafe methods must come from the same origin as the Host they target (CSRF defence).
/// Requests without an Origin header (non-browser clients) pass; browsers always send it on POST.
async fn same_origin(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        let origin = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok());
        let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok());
        if let Some(origin) = origin {
            let origin_host = origin.split("://").nth(1).unwrap_or("");
            if host != Some(origin_host) {
                return ApiError::forbidden("Request origin is not allowed.").into_response();
            }
        }
    }
    next.run(req).await
}

/// Scalar's API reference, loaded from its CDN by the viewing browser.
const DOCS: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>{title} API</title>
<meta name="viewport" content="width=device-width, initial-scale=1"></head>
<body><script id="api-reference" data-url="/api/openapi.json"></script>
<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1"></script></body></html>"#;
