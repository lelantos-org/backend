//! The deadline is the reason this transport exists, so it is pinned here.
//!
//! Alloy's default client has no request timeout, so a call against a node that
//! accepts the connection and never answers hangs forever. Three services shipped
//! that way before `RpcEndpoint` existed.

#![cfg(feature = "rpc")]

use alloy::providers::{Provider, ProviderBuilder};
use chain_types::rpc::{RpcEndpoint, RpcTimeouts};
use std::time::{Duration, Instant};

/// A listener that accepts and then never writes a byte: the hung-node case a
/// connect timeout does not catch.
async fn black_hole() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            // Held open, deliberately unanswered.
            held.push(stream);
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_call_to_a_hung_node_fails_on_the_deadline_instead_of_hanging() {
    let url = black_hole().await;
    let rpc = RpcEndpoint::new(&url, RpcTimeouts::request(1)).expect("endpoint");
    let provider = ProviderBuilder::new().on_client(rpc.client());

    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(30), provider.get_block_number()).await;

    let elapsed = started.elapsed();
    assert!(
        result.is_ok(),
        "the call hung past 30s; the request timeout is not being applied"
    );
    assert!(
        result.unwrap().is_err(),
        "a node that never answers must surface as an error"
    );
    // 1s deadline x (1 + 3 retries) plus backoff, with headroom for a slow CI box.
    assert!(
        elapsed < Duration::from_secs(25),
        "took {elapsed:?}; expected the deadline and retry budget to bound it"
    );
}
