use axkit::{ApiError, ApiResult, State, get};
use axum::Json;
use axum::body::Body;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

struct User;

impl<S: Send + Sync> FromRequestParts<S> for User {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts.headers.contains_key("authorization").then_some(User).ok_or_else(|| ApiError::unauthorized("Sign in."))
    }
}

impl axkit::Security for User {
    const NAME: &'static str = "bearer";
    fn scheme() -> SecurityScheme {
        SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build())
    }
}

#[get("/api/greet/{name}")]
async fn greet(State(prefix): State<&'static str>, Path(name): Path<String>) -> ApiResult<Json<String>> {
    Ok(Json(format!("{prefix} {name}")))
}

#[axkit::post("/api/upload", body_limit = 8)]
async fn upload(body: String) -> String {
    body
}

#[get("/api/private")]
async fn private(_: User) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn post(router: &axum::Router, uri: &str, body: &str) -> StatusCode {
    router.clone().oneshot(Request::post(uri).body(Body::from(body.to_string())).unwrap()).await.unwrap().status()
}

async fn call(router: &axum::Router, uri: &str, auth: bool) -> (StatusCode, String) {
    let mut request = Request::get(uri);
    if auth {
        request = request.header("authorization", "Bearer x");
    }
    let response = router.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn routes_state_security_and_openapi() {
    let router = axkit::Server::new("Test", "1").state("hi").into_router();
    assert_eq!(call(&router, "/api/greet/ana", false).await, (StatusCode::OK, "\"hi ana\"".into()));
    assert_eq!(call(&router, "/api/private", false).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&router, "/api/private", true).await.0, StatusCode::NO_CONTENT);

    assert_eq!(post(&router, "/api/upload", "12345678").await, StatusCode::OK);
    assert_eq!(post(&router, "/api/upload", "123456789").await, StatusCode::PAYLOAD_TOO_LARGE);

    let spec = axkit::openapi("Test", "1");
    assert_eq!(spec["paths"]["/api/greet/{name}"]["get"]["parameters"][0]["name"], "name");
    assert_eq!(spec["paths"]["/api/private"]["get"]["security"][0]["bearer"], serde_json::json!([]));
    assert!(spec["paths"]["/api/greet/{name}"]["get"].get("security").is_none());
    assert_eq!(spec["components"]["securitySchemes"]["bearer"]["scheme"], "bearer");
}
