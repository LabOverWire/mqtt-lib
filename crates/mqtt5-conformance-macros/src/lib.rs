//! Procedural macros for the MQTT v5 conformance test suite.
//!
//! Provides the `#[conformance_test]` attribute, which both registers a
//! conformance test in the `mqtt5_conformance::registry::CONFORMANCE_TESTS`
//! distributed slice and wraps the body in `#[tokio::test]` so the file
//! still works under `cargo test`.
//!
//! Example:
//!
//! ```ignore
//! use mqtt5_conformance::sut::SutHandle;
//! use mqtt5_conformance_macros::conformance_test;
//!
//! #[conformance_test(
//!     ids = ["MQTT-3.12.4-1"],
//!     requires = ["transport.tcp"],
//! )]
//! async fn pingresp_sent_on_pingreq(sut: SutHandle) {
//!     // ...
//! }
//! ```
//!
//! The annotated function MUST be `async`, take a single `SutHandle`
//! argument by value, and return `()`. The macro emits both:
//!
//! 1. A `#[tokio::test]` wrapper that constructs an in-process SUT and
//!    invokes the body — this is what `cargo test` runs.
//! 2. A `static` registration in `CONFORMANCE_TESTS` that the CLI runner
//!    uses to enumerate every test across the workspace at link time.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    Error, Expr, ExprArray, ExprLit, ItemFn, Lit, LitStr, Meta, ReturnType, Token,
};

struct ConformanceTestArgs {
    ids: Vec<LitStr>,
    requires: Vec<LitStr>,
}

impl Parse for ConformanceTestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let metas: Punctuated<Meta, Token![,]> = Punctuated::parse_terminated(input)?;
        let mut ids: Option<Vec<LitStr>> = None;
        let mut requires: Option<Vec<LitStr>> = None;

        for meta in metas {
            let Meta::NameValue(nv) = meta else {
                return Err(Error::new_spanned(
                    meta,
                    "expected `key = [..]` arguments to #[conformance_test]",
                ));
            };
            let key = nv
                .path
                .get_ident()
                .ok_or_else(|| Error::new_spanned(&nv.path, "expected identifier key"))?
                .clone();
            let lits = parse_string_array(&nv.value)?;

            if key == "ids" {
                if ids.is_some() {
                    return Err(Error::new_spanned(key, "duplicate `ids` argument"));
                }
                ids = Some(lits);
            } else if key == "requires" {
                if requires.is_some() {
                    return Err(Error::new_spanned(key, "duplicate `requires` argument"));
                }
                requires = Some(lits);
            } else {
                return Err(Error::new_spanned(
                    key,
                    "unknown key (expected `ids` or `requires`)",
                ));
            }
        }

        let ids =
            ids.ok_or_else(|| Error::new(Span::call_site(), "missing required `ids = [..]`"))?;
        if ids.is_empty() {
            return Err(Error::new(
                Span::call_site(),
                "`ids` must contain at least one statement identifier",
            ));
        }
        Ok(Self {
            ids,
            requires: requires.unwrap_or_default(),
        })
    }
}

fn parse_string_array(value: &Expr) -> syn::Result<Vec<LitStr>> {
    let array: &ExprArray = match value {
        Expr::Array(a) => a,
        other => {
            return Err(Error::new_spanned(
                other,
                "expected an array literal of string literals",
            ));
        }
    };
    let mut out = Vec::with_capacity(array.elems.len());
    for elem in &array.elems {
        let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = elem
        else {
            return Err(Error::new_spanned(
                elem,
                "array elements must be string literals",
            ));
        };
        out.push(s.clone());
    }
    Ok(out)
}

fn validate_id_format(id: &LitStr) -> syn::Result<()> {
    let value = id.value();
    if !value.starts_with("MQTT-") {
        return Err(Error::new_spanned(
            id,
            "conformance id must start with `MQTT-` (e.g. `MQTT-3.12.4-1`)",
        ));
    }
    Ok(())
}

fn validate_require_spec(spec: &LitStr) -> syn::Result<()> {
    let value = spec.value();
    if let Some(rest) = value.strip_prefix("max_qos>=") {
        if !matches!(rest, "0" | "1" | "2") {
            return Err(Error::new_spanned(spec, "max_qos>= must be 0, 1, or 2"));
        }
        return Ok(());
    }
    if value.starts_with("enhanced_auth.") {
        return Ok(());
    }
    match value.as_str() {
        "transport.tcp"
        | "transport.tls"
        | "transport.websocket"
        | "transport.quic"
        | "retain_available"
        | "wildcard_subscription_available"
        | "subscription_identifier_available"
        | "shared_subscription_available"
        | "hooks.restart"
        | "hooks.cleanup"
        | "acl"
        | "strict_client_id_charset"
        | "dollar_sys_publish" => Ok(()),
        _ => Err(Error::new_spanned(
            spec,
            "unknown requirement spec (see Requirement::from_spec for accepted forms)",
        )),
    }
}

fn requirement_to_tokens(spec: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    let value = spec.value();
    let path = quote! { ::mqtt5_conformance::capabilities::Requirement };
    if let Some(rest) = value.strip_prefix("max_qos>=") {
        let n: u8 = match rest {
            "0" => 0,
            "1" => 1,
            "2" => 2,
            _ => {
                return Err(Error::new_spanned(spec, "max_qos>= must be 0, 1, or 2"));
            }
        };
        return Ok(quote! { #path::MinQos(#n) });
    }
    if let Some(method) = value.strip_prefix("enhanced_auth.") {
        return Ok(quote! { #path::EnhancedAuthMethod(#method) });
    }
    let variant = match value.as_str() {
        "transport.tcp" => quote! { TransportTcp },
        "transport.tls" => quote! { TransportTls },
        "transport.websocket" => quote! { TransportWebSocket },
        "transport.quic" => quote! { TransportQuic },
        "retain_available" => quote! { RetainAvailable },
        "wildcard_subscription_available" => quote! { WildcardSubscriptionAvailable },
        "subscription_identifier_available" => quote! { SubscriptionIdentifierAvailable },
        "shared_subscription_available" => quote! { SharedSubscriptionAvailable },
        "hooks.restart" => quote! { HookRestart },
        "hooks.cleanup" => quote! { HookCleanup },
        "acl" => quote! { Acl },
        "strict_client_id_charset" => quote! { StrictClientIdCharset },
        "dollar_sys_publish" => quote! { DollarSysPublish },
        _ => {
            return Err(Error::new_spanned(
                spec,
                "unknown requirement spec (see Requirement::from_spec for accepted forms)",
            ));
        }
    };
    Ok(quote! { #path::#variant })
}

fn validate_signature(item_fn: &ItemFn) -> syn::Result<()> {
    if item_fn.sig.asyncness.is_none() {
        return Err(Error::new_spanned(
            &item_fn.sig,
            "#[conformance_test] requires an async fn",
        ));
    }
    if item_fn.sig.inputs.len() != 1 {
        return Err(Error::new_spanned(
            &item_fn.sig.inputs,
            "#[conformance_test] async fn must take exactly one argument: `sut: SutHandle`",
        ));
    }
    if !matches!(item_fn.sig.output, ReturnType::Default) {
        return Err(Error::new_spanned(
            &item_fn.sig.output,
            "#[conformance_test] async fn must return ()",
        ));
    }
    Ok(())
}

#[proc_macro_attribute]
pub fn conformance_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ConformanceTestArgs);
    let item_fn = parse_macro_input!(item as ItemFn);

    if let Err(e) = validate_signature(&item_fn) {
        return e.to_compile_error().into();
    }
    for id in &args.ids {
        if let Err(e) = validate_id_format(id) {
            return e.to_compile_error().into();
        }
    }
    for req in &args.requires {
        if let Err(e) = validate_require_spec(req) {
            return e.to_compile_error().into();
        }
    }

    let user_name = item_fn.sig.ident.clone();
    let user_name_str = user_name.to_string();
    let impl_ident = format_ident!("__conformance_impl_{}", user_name_str);
    let runner_ident = format_ident!("__conformance_runner_{}", user_name_str);
    let registry_ident = format_ident!("__CONFORMANCE_TEST_{}", user_name_str.to_uppercase());

    let mut impl_fn = item_fn.clone();
    impl_fn.sig.ident = impl_ident.clone();
    impl_fn.vis = syn::Visibility::Inherited;

    let ids = args.ids.iter().map(|s| {
        let v = s.value();
        quote! { #v }
    });
    let mut requires_tokens = Vec::with_capacity(args.requires.len());
    for spec in &args.requires {
        match requirement_to_tokens(spec) {
            Ok(tokens) => requires_tokens.push(tokens),
            Err(e) => return e.to_compile_error().into(),
        }
    }

    let expanded = quote! {
        #impl_fn

        #[cfg(test)]
        #[::tokio::test]
        async fn #user_name() {
            let sut = ::mqtt5_conformance::sut::inprocess_sut_with_websocket().await;
            #impl_ident(sut).await;
        }

        fn #runner_ident(
            sut: ::mqtt5_conformance::sut::SutHandle,
        ) -> ::mqtt5_conformance::registry::TestFuture {
            ::std::boxed::Box::pin(async move {
                #impl_ident(sut).await;
            })
        }

        #[::mqtt5_conformance::registry::linkme::distributed_slice(
            ::mqtt5_conformance::registry::CONFORMANCE_TESTS
        )]
        #[linkme(crate = ::mqtt5_conformance::registry::linkme)]
        static #registry_ident: ::mqtt5_conformance::registry::ConformanceTest =
            ::mqtt5_conformance::registry::ConformanceTest {
                name: #user_name_str,
                module_path: ::std::module_path!(),
                file: ::std::file!(),
                line: ::std::line!(),
                ids: &[#(#ids),*],
                requires: &[#(#requires_tokens),*],
                runner: #runner_ident,
            };
    };

    expanded.into()
}
