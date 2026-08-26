//! Request span shared by every webserver's `TraceLayer`.
//!
//! Separate from [`crate::http`], which owns the error mapping.

use axum::http::Request;
use tower_http::classify::{ServerErrorsAsFailures, SharedClassifier};
use tower_http::trace::{MakeSpan, TraceLayer};
use tracing::{Span, info_span};

/// Builds a request span carrying the method and path, never the query string.
///
/// `tower_http`'s `DefaultMakeSpan` records the full URI. Query strings here are
/// per-caller access patterns: `after` on `/v1/matches` is a wallet's position
/// in the note feed, and a sequence of them describes that wallet's sync
/// history. `DefaultMakeSpan` records at DEBUG, so a single `RUST_LOG=debug`
/// would enable query-string logging across every service at once. The path
/// alone is sufficient to debug routing.
///
/// Generic over the body type rather than pinned to `axum::body::Body`, so it
/// composes with a `TraceLayer` anywhere in a stack.
#[derive(Clone, Copy, Debug, Default)]
pub struct PathOnly;

impl<B> MakeSpan<B> for PathOnly {
    fn make_span(&mut self, req: &Request<B>) -> Span {
        info_span!("http", method = %req.method(), path = %req.uri().path())
    }
}

/// The layer type [`trace_layer`] returns.
///
/// Named so a router can store or pass one without spelling out `tower_http`'s
/// classifier generics.
pub type PathOnlyTrace = TraceLayer<SharedClassifier<ServerErrorsAsFailures>, PathOnly>;

/// `TraceLayer` that records the path only.
///
/// Every router should use `.layer(shared::request_span::trace_layer())` so a
/// new webserver cannot pick up `DefaultMakeSpan` by omission.
pub fn trace_layer() -> PathOnlyTrace {
    TraceLayer::new_for_http().make_span_with(PathOnly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::fmt::format::FmtSpan;

    /// Collects what the `fmt` layer writes.
    ///
    /// The guarantee under test concerns what reaches a log sink. A span's
    /// fields are rendered only there; a `Span` handle exposes no way to read
    /// back what was recorded on it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Everything a subscriber would print for a request to `uri`.
    fn emit(uri: &str) -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_span_events(FmtSpan::NEW)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let req = Request::builder().uri(uri).body(()).unwrap();
            let _entered = PathOnly.make_span(&req).entered();
        });

        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn records_method_and_path() {
        let out = emit("/v1/matches?chainId=1");
        assert!(out.contains("/v1/matches"), "path missing: {out}");
        assert!(out.contains("GET"), "method missing: {out}");
    }

    /// Covers a paging cursor, a chain id, and an address reaching a URL
    /// despite the POST-only convention on `/v1/screen`.
    #[test]
    fn omits_the_query_string() {
        const ADDRESS: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let cases = [
            ("/v1/matches?chainId=1&after=90210&limit=500", "90210"),
            ("/v1/matches?chainId=1&after=90210&limit=500", "chainId"),
            ("/v1/notes?after=41", "after"),
            (
                concat!(
                    "/v1/screen?address=",
                    "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                ),
                ADDRESS,
            ),
        ];

        for (uri, secret) in cases {
            let out = emit(uri);
            assert!(!out.contains(secret), "`{secret}` reached the log: {out}");
            assert!(!out.contains('?'), "query string reached the log: {out}");
        }
    }
}
