use std::marker::PhantomData;

use utoipa::openapi::security::{SecurityRequirement, SecurityScheme};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

/// An authentication extractor. Every route taking one as an argument is documented as
/// requiring it, and its scheme is added to the OpenAPI document. Enforcement stays in the
/// extractor; this only describes it. `Option<T>` arguments are not treated as required.
///
/// ```ignore
/// impl appkit::Security for Admin {
///     const NAME: &'static str = "session";
///     fn scheme() -> SecurityScheme {
///         SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("my_session")))
///     }
/// }
/// ```
pub trait Security {
    /// Key under `components.securitySchemes`.
    const NAME: &'static str;
    /// Scopes the route requires (OAuth2/OpenID Connect); empty for other schemes.
    const SCOPES: &'static [&'static str] = &[];
    /// How clients authenticate: a cookie, header, HTTP bearer, OAuth2 flow, ...
    fn scheme() -> SecurityScheme;
}

/// `(name, scopes, scheme)` of one `Security` argument.
#[doc(hidden)]
pub type Found = (&'static str, &'static [&'static str], SecurityScheme);

/// Asks at compile time whether a handler argument implements `Security`, using method
/// resolution: `(&Probe::<T>(PhantomData)).security()` picks `Secured` when `T: Security`
/// and falls back to `Unsecured` (one auto-ref further) otherwise.
#[doc(hidden)]
pub struct Probe<T>(pub PhantomData<T>);

#[doc(hidden)]
pub trait Secured {
    fn security(&self) -> Option<Found>;
}

impl<T: Security> Secured for Probe<T> {
    fn security(&self) -> Option<Found> {
        Some((T::NAME, T::SCOPES, T::scheme()))
    }
}

#[doc(hidden)]
pub trait Unsecured {
    fn security(&self) -> Option<Found> {
        None
    }
}

impl<T> Unsecured for &Probe<T> {}

/// Mounts one macro-generated route and records the security its arguments require.
/// An explicit `security(...)` on the route attribute wins.
#[doc(hidden)]
pub fn register(mut router: OpenApiRouter, (schemas, mut paths, method): UtoipaMethodRouter, security: Vec<Found>) -> OpenApiRouter {
    if !security.is_empty() {
        let components = router.get_openapi_mut().components.get_or_insert_with(Default::default);
        let mut requirement = SecurityRequirement::default();
        for (name, scopes, scheme) in security {
            components.add_security_scheme(name, scheme);
            requirement = requirement.add(name, scopes.iter().copied());
        }
        for item in paths.paths.values_mut() {
            let operations = [&mut item.get, &mut item.put, &mut item.post, &mut item.delete, &mut item.patch];
            for operation in operations.into_iter().flatten() {
                operation.security.get_or_insert_with(|| vec![requirement.clone()]);
            }
        }
    }
    router.routes((schemas, paths, method))
}
