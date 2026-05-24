//! Shared helpers for constructing alloy RPC providers and running an
//! operation against the first working RPC out of a list.
//!
//! Two modules need the same pattern — [`crate::wrapped_rates::RpcRateFetcher`]
//! when reading `convertToAssets` and [`crate::wrapped_donations::scan_for_wrapper`]
//! when walking ERC4626 logs. Both modules previously open-coded
//! `ProviderBuilder::new().connect_http(rpc.clone())` plus a fallback loop;
//! this module centralises both.
//!
//! The provider type returned by `ProviderBuilder::new().connect_http(...)` is
//! an opaque alloy type with a long generic signature, so we surface the
//! "build then run" pattern as [`try_with_providers`] rather than returning the
//! provider out of a constructor function. Callers pass a closure that takes
//! a `&Url` and returns whatever they need; the helper threads the per-RPC
//! error reporting and the "stop on first success" semantics.

use std::future::Future;
use url::Url;

/// Error string produced when the supplied RPC list is empty.
pub(crate) const NO_RPCS_CONFIGURED: &str = "no rpc urls configured";

/// Run `operation` against each `Url` in `rpcs` in order, returning the first
/// `Ok(value)`. When every RPC fails (or `rpcs` is empty) the last error
/// string is surfaced via `on_exhausted`, allowing the caller to map it onto
/// a domain-specific error variant.
///
/// `operation` is invoked with a `&Url` rather than a pre-built provider so
/// the caller can construct whatever shape of provider they need (e.g.
/// `ProviderBuilder::new().connect_http(url.clone())`) without this helper
/// having to spell out the long generic signature of the alloy provider.
///
/// Failure callbacks are kept minimal — callers tracing the per-RPC retry
/// can do so inside `operation` itself.
pub(crate) async fn try_with_providers<'a, T, E, F, Fut>(
    rpcs: &'a [Url],
    mut operation: F,
) -> Result<T, String>
where
    E: std::fmt::Display,
    F: FnMut(&'a Url) -> Fut,
    Fut: Future<Output = Result<T, E>> + 'a,
{
    if rpcs.is_empty() {
        return Err(NO_RPCS_CONFIGURED.to_string());
    }
    let mut last_err: Option<String> = None;
    for rpc in rpcs {
        match operation(rpc).await {
            Ok(value) => return Ok(value),
            Err(e) => {
                last_err = Some(e.to_string());
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "all rpcs failed".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[rocket::async_test]
    async fn returns_first_success() {
        let urls = vec![
            "http://a.invalid/".parse::<Url>().unwrap(),
            "http://b.invalid/".parse::<Url>().unwrap(),
        ];
        let calls = Cell::new(0u32);
        let result: Result<u32, String> = try_with_providers(&urls, |_url| async {
            calls.set(calls.get() + 1);
            Ok::<u32, String>(42)
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.get(), 1, "should short-circuit on first success");
    }

    #[rocket::async_test]
    async fn falls_through_to_second_on_first_failure() {
        let urls = vec![
            "http://a.invalid/".parse::<Url>().unwrap(),
            "http://b.invalid/".parse::<Url>().unwrap(),
        ];
        let calls = Cell::new(0u32);
        let result: Result<&'static str, String> = try_with_providers(&urls, |_url| async {
            let n = calls.get();
            calls.set(n + 1);
            if n == 0 {
                Err::<&'static str, String>("first failed".to_string())
            } else {
                Ok("second worked")
            }
        })
        .await;
        assert_eq!(result.unwrap(), "second worked");
        assert_eq!(calls.get(), 2);
    }

    #[rocket::async_test]
    async fn surfaces_last_error_when_all_fail() {
        let urls = vec![
            "http://a.invalid/".parse::<Url>().unwrap(),
            "http://b.invalid/".parse::<Url>().unwrap(),
        ];
        let result: Result<(), String> = try_with_providers(&urls, |url| async move {
            Err::<(), String>(format!("boom@{url}"))
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("http://b.invalid/"),
            "expected last url error, got {err}"
        );
    }

    #[rocket::async_test]
    async fn empty_rpcs_errors_immediately() {
        let urls: Vec<Url> = vec![];
        let result: Result<(), String> =
            try_with_providers(&urls, |_url| async { Ok::<(), String>(()) }).await;
        assert_eq!(result.unwrap_err(), NO_RPCS_CONFIGURED);
    }
}
