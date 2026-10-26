//! `#[get("/path")]`, `#[post(...)]`, ... for axkit.
//!
//! Each attribute keeps the handler as written and adds:
//! 1. `#[axkit::utoipa::path(...)]`, inferred from the signature (params, body, responses, tag);
//! 2. an `inventory` entry so `axkit::Server` mounts the route and documents it, including
//!    the security of every argument that implements `axkit::Security`.
//!
//! Inference reads syntax, not resolved types, so it recognises wrapper names exactly:
//! `Path`, `Query`, `Json`, `Form`, `Multipart`, `ApiResult`/`Result`, `Json<T>`, `StatusCode` (204).
//! Security is the exception: it is resolved by the compiler through `axkit::Security`.
//! For handlers returning `impl IntoResponse`, name the success body with `ok = Type`.
//! `body_limit = bytes` raises (or lowers) the server's request body limit for this route only.
//! Anything else can be passed through: `#[post("/x", responses(...), request_body = ...)]`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::{ToTokens, format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{FnArg, GenericArgument, ItemFn, LitStr, Meta, PathArguments, ReturnType, Token, Type, parse_macro_input};

struct Args {
    path: LitStr,
    ok: Option<Type>,
    body_limit: Option<syn::Expr>,
    passthrough: Vec<Meta>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let path: LitStr = input.parse()?;
        let mut ok = None;
        let mut body_limit = None;
        let mut passthrough = Vec::new();
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            for meta in Punctuated::<Meta, Token![,]>::parse_terminated(input)? {
                match meta {
                    Meta::NameValue(nv) if nv.path.is_ident("ok") => ok = Some(syn::parse2(nv.value.to_token_stream())?),
                    Meta::NameValue(nv) if nv.path.is_ident("body_limit") => body_limit = Some(nv.value),
                    other => passthrough.push(other),
                }
            }
        }
        Ok(Self { path, ok, body_limit, passthrough })
    }
}

macro_rules! method {
    ($($name:ident),*) => {$(
        #[proc_macro_attribute]
        pub fn $name(args: TokenStream, item: TokenStream) -> TokenStream {
            let args = parse_macro_input!(args as Args);
            let item = parse_macro_input!(item as ItemFn);
            expand(stringify!($name), args, item).unwrap_or_else(|e| e.to_compile_error()).into()
        }
    )*};
}
method!(get, post, put, patch, delete);

fn expand(method: &str, args: Args, item: ItemFn) -> syn::Result<Tokens> {
    let path = args.path.value();
    if !path.starts_with('/') {
        return Err(syn::Error::new_spanned(&args.path, "route path must start with `/`"));
    }
    let given = |key: &str| args.passthrough.iter().any(|m| m.path().is_ident(key));
    let mut attrs: Vec<Tokens> = Vec::new();
    let mut params: Vec<Tokens> = Vec::new();
    let mut body: Option<Tokens> = None;
    let mut types: Vec<&Type> = Vec::new();

    for input in &item.sig.inputs {
        let FnArg::Typed(arg) = input else { continue };
        types.push(&arg.ty);
        let Some((name, inner)) = wrapper(&arg.ty) else { continue };
        match (name.as_str(), inner) {
            ("Path", Some(inner)) => {
                if is_params_struct(inner) {
                    params.push(inner.to_token_stream());
                } else {
                    params.extend(template_names(&path).into_iter().map(|n| quote!((#n, Path))));
                }
            }
            ("Query", Some(inner)) => params.push(inner.to_token_stream()),
            ("Json", Some(inner)) => body = Some(quote!(request_body = #inner)),
            ("Form", Some(inner)) => {
                body = Some(quote!(request_body(content = #inner, content_type = "application/x-www-form-urlencoded")))
            }
            ("Multipart", _) => body = Some(quote!(request_body(content_type = "multipart/form-data"))),
            _ => {}
        }
    }

    if !given("params") && !params.is_empty() {
        attrs.push(quote!(params(#(#params),*)));
    }
    if let Some(body) = body.filter(|_| !given("request_body")) {
        attrs.push(body);
    }
    if !given("responses") {
        let ok = match &args.ok {
            Some(ty) => quote!((status = 200, description = "Success", body = #ty)),
            None => success(&item.sig.output),
        };
        attrs.push(quote!(responses(
            #ok,
            (status = "4XX", description = "Request rejected", body = ::axkit::ApiError),
            (status = "5XX", description = "Server error", body = ::axkit::ApiError),
        )));
    }
    if !given("tag") && !given("tags") {
        let tag = tag_for(&path);
        attrs.push(quote!(tag = #tag));
    }
    for meta in &args.passthrough {
        attrs.push(meta.to_token_stream());
    }

    let method_ident = format_ident!("{}", method);
    let ident = &item.sig.ident;
    let lit = &args.path;
    let body_limit = match &args.body_limit {
        Some(limit) => quote!(::core::option::Option::Some(#limit)),
        None => quote!(::core::option::Option::None),
    };
    Ok(quote! {
        #[::axkit::utoipa::path(#method_ident, path = #lit, #(#attrs),*)]
        #item

        ::axkit::inventory::submit! {
            ::axkit::Route {
                register: |router| {
                    use ::axkit::__private::{Secured as _, Unsecured as _};
                    let mut security = ::std::vec::Vec::new();
                    #(security.extend((&::axkit::__private::Probe::<#types>(::core::marker::PhantomData)).security());)*
                    let route = {
                        // `routes!` names `axum` and `utoipa` unqualified; resolve them through axkit.
                        use ::axkit::{axum, utoipa};
                        ::axkit::utoipa_axum::routes!(#ident)
                    };
                    ::axkit::__private::register(router, route, security, #body_limit)
                },
            }
        }
    })
}

/// Outer type name and its first generic argument: `Path<T>` → ("Path", Some(T)).
/// Handles pattern-style args like `Path(id): Path<String>` because the type is inspected.
fn wrapper(ty: &Type) -> Option<(String, Option<&Type>)> {
    let Type::Path(tp) = ty else { return None };
    let seg = tp.path.segments.last()?;
    let inner = match &seg.arguments {
        PathArguments::AngleBracketed(a) => a.args.iter().find_map(|g| match g {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        }),
        _ => None,
    };
    Some((seg.ident.to_string(), inner))
}

const SCALARS: &[&str] = &["String", "str", "Uuid", "i32", "i64", "u32", "u64", "usize", "bool"];

fn is_params_struct(ty: &Type) -> bool {
    match ty {
        Type::Path(tp) => tp.path.segments.last().is_some_and(|s| !SCALARS.contains(&s.ident.to_string().as_str())),
        _ => false,
    }
}

fn template_names(path: &str) -> Vec<String> {
    path.split('/')
        .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .map(|s| s.trim_start_matches('*').to_string())
        .collect()
}

/// `/api/admin/tags/{id}` → `tags`; `/api/session` → `session`.
fn tag_for(path: &str) -> String {
    path.split('/').find(|s| !s.is_empty() && !s.starts_with('{') && *s != "api" && *s != "admin").unwrap_or("default").to_string()
}

fn success(output: &ReturnType) -> Tokens {
    let ReturnType::Type(_, ty) = output else {
        return quote!((status = 200, description = "Done"));
    };
    // Unwrap `ApiResult<T>` / `Result<T, E>`.
    let ty = match wrapper(ty) {
        Some((name, Some(inner))) if name == "ApiResult" || name == "Result" => inner,
        _ => ty,
    };
    match wrapper(ty) {
        Some((name, Some(inner))) if name == "Json" => quote!((status = 200, description = "Success", body = #inner)),
        Some((name, _)) if name == "StatusCode" => quote!((status = 204, description = "Done")),
        _ => quote!((status = 200, description = "Done")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand_str(method: &str, args: &str, item: &str) -> String {
        let args: Args = syn::parse_str(args).unwrap();
        let item: ItemFn = syn::parse_str(item).unwrap();
        expand(method, args, item).unwrap().to_string()
    }

    #[test]
    fn infers_params_body_responses_security_and_tag() {
        let out = expand_str(
            "post",
            r#""/api/admin/tags/{id}""#,
            "async fn save_tag(_a: Admin, Path(id): Path<String>, Json(b): Json<TagInput>) -> ApiResult<Json<Tag>> { todo!() }",
        );
        assert!(out.contains("params ((\"id\" , Path))"), "{out}");
        assert!(out.contains("request_body = TagInput"), "{out}");
        assert!(out.contains("status = 200") && out.contains("body = Tag"), "{out}");
        assert!(out.contains("Probe :: < Admin >") && out.contains("Probe :: < Path < String > >"), "{out}");
        assert!(out.contains("tag = \"tags\""), "{out}");
        assert!(out.contains("inventory :: submit"), "{out}");
    }

    #[test]
    fn query_struct_and_unit_results_are_200_without_body() {
        let out =
            expand_str("get", r#""/api/equipment""#, "async fn get_equipment(Query(q): Query<EquipmentPage>) -> ApiResult<()> { todo!() }");
        assert!(out.contains("params (EquipmentPage)"), "{out}");
        assert!(out.contains(r#"(status = 200 , description = "Done")"#), "{out}");
    }

    #[test]
    fn passthrough_overrides_inference() {
        let out = expand_str(
            "get",
            r#""/api/media/{id}/image", responses((status = 200, content_type = "image/png"))"#,
            "async fn get_map_image(Path(id): Path<Uuid>) -> Response { todo!() }",
        );
        assert_eq!(out.matches("responses").count(), 1, "{out}");
        assert!(out.contains("image/png"), "{out}");
    }

    #[test]
    fn ok_names_the_success_body_and_keeps_error_inference() {
        let out = expand_str(
            "post",
            r#""/api/auth/sign-in", ok = Done"#,
            "async fn sign_in(Json(c): Json<Credentials>) -> ApiResult<impl IntoResponse> { todo!() }",
        );
        assert!(out.contains("body = Done") && out.contains("4XX") && !out.contains("ok ="), "{out}");
    }

    #[test]
    fn form_bodies_are_url_encoded() {
        let out = expand_str(
            "post",
            r#""/api/auth/sign-in""#,
            "async fn sign_in(Form(c): Form<Credentials>) -> ApiResult<Json<Done>> { todo!() }",
        );
        assert!(out.contains("content = Credentials") && out.contains("application/x-www-form-urlencoded"), "{out}");
    }

    #[test]
    fn rejects_relative_paths() {
        let args: Args = syn::parse_str(r#""api/x""#).unwrap();
        let item: ItemFn = syn::parse_str("async fn x() {}").unwrap();
        assert!(expand("get", args, item).is_err());
    }
}
