use std::future::Future;
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
use utoipa::openapi::OpenApi;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa_axum::router::OpenApiRouter;

use crate::{ApiError, Route};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Task = Box<dyn FnOnce(CancellationToken) -> BoxFuture<Result<(), String>> + Send>;
type Check = Box<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>;

/// Builds the OpenAPI document from every `#[get]`/`#[post]`/... route in the binary.
pub fn openapi(title: &str, version: &str, session_cookie: Option<&str>) -> OpenApi {
    collect(title, version, session_cookie).1
}

fn collect(title: &str, version: &str, session_cookie: Option<&str>) -> (Router, OpenApi) {
    let mut routes = OpenApiRouter::new();
    for route in inventory::iter::<Route> {
        routes = (route.register)(routes);
    }
    let (router, mut api) = routes.split_for_parts();
    api.info.title = title.to_string();
    api.info.version = version.to_string();
    api.info.description = None;
    api.info.license = None;
    api.info.contact = None;
    if let Some(cookie) = session_cookie {
        api.components
            .get_or_insert_with(Default::default)
            .add_security_scheme("session", SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(cookie))));
    }
    (router, api)
}

/// HTTP server with auto-registered routes, OpenAPI, docs, health checks and background tasks.
pub struct Server {
    title: String,
    version: String,
    session_cookie: Option<String>,
    docs_script: Option<String>,
    body_limit: usize,
    timeout: Duration,
    extra: Router,
    layers: Vec<Box<dyn FnOnce(Router) -> Router + Send>>,
    tasks: Vec<(String, Task)>,
    checks: Vec<(String, Check)>,
    shutdown: CancellationToken,
}

impl Server {
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            session_cookie: None,
            docs_script: Some("https://cdn.jsdelivr.net/npm/@scalar/api-reference".into()),
            body_limit: 2 * 1024 * 1024,
            timeout: Duration::from_secs(30),
            extra: Router::new(),
            layers: Vec::new(),
            tasks: Vec::new(),
            checks: Vec::new(),
            shutdown: CancellationToken::new(),
        }
    }

    /// Makes `appkit::State<T>` available to every route.
    pub fn state<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
        self.layers.push(Box::new(move |r| r.layer(axum::Extension(value))));
        self
    }

    /// Documents cookie-based session auth as the `session` security scheme.
    pub fn session_cookie(mut self, name: impl Into<String>) -> Self {
        self.session_cookie = Some(name.into());
        self
    }

    /// Script URL for the docs page at `/api/docs`; `None` disables the page.
    /// Serve the Scalar standalone bundle yourself to work without internet.
    pub fn docs(mut self, script_url: Option<String>) -> Self {
        self.docs_script = script_url;
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

    /// Token cancelled when the server begins shutting down.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// The fully assembled router, without serving it (used by tests).
    pub fn into_router(self) -> Router {
        self.build().0
    }

    fn build(self) -> (Router, Vec<(String, Task)>, CancellationToken) {
        let (api, openapi) = collect(&self.title, &self.version, self.session_cookie.as_deref());
        let api = api
            .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store")))
            .layer(middleware::from_fn(same_origin));
        let spec = serde_json::to_string(&openapi).expect("OpenAPI serializes");
        let mut router = api.route("/api/openapi.json", get(move || async move { ([(header::CONTENT_TYPE, "application/json")], spec) }));
        if let Some(script) = self.docs_script {
            let page = Html(DOCS.replace("{title}", &self.title).replace("{script}", &script));
            router = router.route("/api/docs", get(move || async move { page }));
        }
        let checks = std::sync::Arc::new(self.checks);
        router = router
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
            .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, self.timeout))
            .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
                let route = req.extensions().get::<MatchedPath>().map(|p| p.as_str()).unwrap_or("-");
                tracing::info_span!("http", method = %req.method(), route, path = %req.uri().path())
            }));
        (router, self.tasks, self.shutdown)
    }

    /// Serves until Ctrl-C/SIGTERM or until a task fails, then drains in-flight work.
    pub async fn serve<L>(self, listener: L) -> Result<(), String>
    where
        L: axum::serve::Listener<Addr = std::net::SocketAddr>,
    {
        use axum::serve::ListenerExt;
        // `tap_io` gives any listener (plain TCP or TLS) axum's ConnectInfo support.
        let listener = listener.tap_io(|_| {});
        let (router, tasks, token) = self.build();
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
        let signal = token.clone();
        tokio::spawn(async move {
            tokio::select! { _ = shutdown_signal() => {}, _ = signal.cancelled() => {} }
            signal.cancel();
        });
        let stop = token.clone();
        let served = axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .with_graceful_shutdown(async move { stop.cancelled().await })
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

const DOCS: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>{title} API</title>
<meta name="viewport" content="width=device-width, initial-scale=1"></head>
<body><script id="api-reference" data-url="/api/openapi.json"></script>
<script src="{script}"></script></body></html>"#;
