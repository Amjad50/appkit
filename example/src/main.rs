use axkit::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct App {
    greeting: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct Hello {
    message: String,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct Shout {
    /// Upper-case the greeting.
    #[serde(default)]
    loud: bool,
}

/// Says hello. Doc comments become the OpenAPI summary and description.
#[get("/api/hello/{name}")]
async fn hello(State(app): State<App>, Path(name): Path<String>, Query(shout): Query<Shout>) -> ApiResult<Json<Hello>> {
    if name.trim().is_empty() {
        return Err(ApiError::bad_request("Name is required."));
    }
    let message = format!("{}, {name}", app.greeting);
    Ok(Json(Hello { message: if shout.loud { message.to_uppercase() } else { message } }))
}

#[post("/api/echo")]
async fn echo(Json(body): Json<Hello>) -> Json<Hello> {
    Json(body)
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let _logs = axkit::telemetry::init("hello", &Default::default());
    axkit::Server::new("Hello", env!("CARGO_PKG_VERSION"))
        .state(App { greeting: "Hello".into() })
        .serve("127.0.0.1:3000".parse().unwrap())
        .await
}
