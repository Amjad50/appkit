# appkit

A small layer over [axum](https://docs.rs/axum). You annotate plain axum handlers, and appkit
registers them, documents them in OpenAPI and serves them, with config, logging, background tasks
and graceful shutdown built in. It is small enough to read in one sitting, and it is meant to be
easy for people and coding agents to use correctly.

```toml
[dependencies]
appkit = { git = "https://github.com/Amjad50/appkit" }
axum = "0.8"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
utoipa = "6"
```

```rust,no_run
use appkit::{ApiError, ApiResult, State, get, post};
use axum::Json;
use axum::extract::Path;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone)]
struct App {
    greeting: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct Hello {
    message: String,
}

/// Says hello. Doc comments become the OpenAPI summary and description.
#[get("/api/hello/{name}")]
async fn hello(State(app): State<App>, Path(name): Path<String>) -> ApiResult<Json<Hello>> {
    if name.is_empty() {
        return Err(ApiError::bad_request("Name is required."));
    }
    Ok(Json(Hello { message: format!("{}, {name}", app.greeting) }))
}

#[post("/api/echo")]
async fn echo(Json(body): Json<Hello>) -> Json<Hello> {
    Json(body)
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let _logs = appkit::telemetry::init("hello", &Default::default());
    appkit::Server::new("Hello", env!("CARGO_PKG_VERSION"))
        .state(App { greeting: "Hello".into() })
        .serve("127.0.0.1:3000".parse().unwrap())
        .await
}
```

Every route marked with `#[get]`, `#[post]`, `#[put]`, `#[patch]` or `#[delete]` anywhere in the
binary is mounted automatically, so there is no router to keep in sync.

## What you get

| Path                | What                                                                 |
| ------------------- | -------------------------------------------------------------------- |
| your routes         | auto-registered, each request span named `METHOD /route/{param}`    |
| `/api/openapi.json` | OpenAPI 3.1 document of every route                                  |
| `/api/docs`         | interactive API reference ([Scalar](https://scalar.com), from its CDN) |
| `/healthz`          | always `ok`                                                          |
| `/readyz`           | runs every `.ready(...)` check, 503 with the failing check's name otherwise |

These defaults apply to every request: a 2 MiB body limit (`.body_limit(bytes)`), a 30 s timeout,
gzip, a `Cache-Control: private, no-store` header on API routes, and a same-origin check on unsafe
methods (CSRF defence: if a browser sends an `Origin`, it must match `Host`).

## Route macros

The macro reads the handler signature and writes `#[utoipa::path]` for you:

- `Path<T>`, `Query<T>` → parameters. Scalars are named after the `{segments}` of the path; a
  struct must derive `IntoParams`.
- `Json<T>` → JSON request body, `Form<T>` → url-encoded body, `Multipart` → multipart body.
- Return `Json<T>` / `ApiResult<Json<T>>` → 200 with `T`. `StatusCode` → 204 (return
  `StatusCode::NO_CONTENT`). `()` and anything else (`impl IntoResponse`, `Response`) → 200
  without a body; name the body with `ok = Type`.
- Every route also documents `4XX`/`5XX` with `ApiError`.
- Tag: the first path segment after `/api` (and `/admin`), so `/api/admin/users/{id}` → `users`.
- Anything else is passed through to utoipa and overrides inference:
  `#[get("/api/file/{id}", responses((status = 200, content_type = "image/png")))]`.

Response types usually derive `Serialize` and `ToSchema`. In the generated document every field of
a response-only schema is `required`, because serde always writes it (as `null` when it is `None`).
This way generated clients get `T | null` instead of `T | null | undefined`.

## State

`Server::state(value)` makes `State<T>` available to every handler. Call it once for each type you
need. `T` must be `Clone`, so wrap shared data in `Arc`.

## Errors

`ApiError { status, message }` is sent as `{ "message": "..." }` with the status code. Use
`ApiError::bad_request`, `unauthorized`, `forbidden`, `not_found`, `conflict`, or `new(status, msg)`.
`?` converts any other error: extractor rejections (bad JSON, query, path, form or multipart data)
keep their status and message, and everything else is logged and becomes a generic 500, so
internals never reach clients.

## Authentication

Enforce authentication in an axum extractor, then implement `appkit::Security` for it. Every route
that takes the extractor as an argument is then documented as requiring it, and the scheme is added
to the document. The "Authorize" button in `/api/docs` uses it.

```rust,ignore
use utoipa::openapi::security::{Flow, OAuth2, Password, Scopes, SecurityScheme};

impl appkit::Security for Admin {
    const NAME: &'static str = "session";
    fn scheme() -> SecurityScheme {
        SecurityScheme::OAuth2(OAuth2::new([Flow::Password(Password::new("/api/auth/sign-in", Scopes::new()))]))
    }
}
```

`Option<Admin>` arguments are optional, so they are not documented as required.

## Config

```rust,ignore
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Config { http: Http, telemetry: appkit::telemetry::Config }

let config: Config = appkit::config::load("MYAPP", &cli_overrides)?;
```

Layers, from lowest to highest priority:

1. `Default`;
2. environment variables `MYAPP__SECTION__KEY=value` (a `.env` file is loaded first, and real
   variables win over it);
3. `section.key=value` strings, for example from a `--set` flag.

## Background tasks and shutdown

```rust,ignore
server.task("worker", |stop: appkit::CancellationToken| async move {
    stop.cancelled().await;
    Ok::<_, String>(())
})
```

A task must return when its token is cancelled. If any task returns an error, the whole server shuts
down. On Ctrl-C or SIGTERM the server stops accepting connections, cancels the token and waits up to
10 s for open connections and tasks. Handlers can take `Extension<CancellationToken>` to end
long-lived responses (server-sent events, streams) at that point.

## Logs and telemetry

`telemetry::init(service, &config)` writes `pretty` or `json` logs. `RUST_LOG` overrides
`config.filter`. With the `otel` feature enabled and `OTEL_EXPORTER_OTLP_ENDPOINT` set, spans and
metrics are also exported over OTLP/gRPC, and `appkit::meter(name)` gives you a metrics meter. Keep
the returned guard alive until exit.

## Tests

`Server::into_router()` gives you the assembled `axum::Router` without binding a port, so you can
drive it with `tower::ServiceExt::oneshot`. `appkit::openapi(title, version)` returns the document,
for example to write `openapi.json` for a client generator.

## License

MIT
