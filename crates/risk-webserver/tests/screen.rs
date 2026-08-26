//! End-to-end screening over a real Postgres.
//!
//! The service has no write API, so fixtures are inserted directly, which is also
//! how the list is populated in production. That exercises the contract that the
//! stored form is already normalized, which a write endpoint would hide.

use database::schema::screened_addresses;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use risk_webserver::{RiskWebserverConfig, build_router, build_state};
use std::sync::Arc;

/// Same address in the two spellings a caller might send.
const EVM_CHECKSUMMED: &str = "0x8589427373D6D84E98730D7795D8f6f8731FDA16";
const EVM_LOWER: &str = "0x8589427373d6d84e98730d7795d8f6f8731fda16";
const EVM_CLEAN: &str = "0x0000000000000000000000000000000000000001";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screen_matches_checksummed_input_against_lowercase_row() {
    let (pool, _guard) = fresh_pool().await;
    insert_entry(&pool, "evm", EVM_LOWER, "banned", "ofac_sdn", Some("SDN")).await;
    let base = spawn(&pool).await;

    let body = screen_one(&base, "evm", EVM_CHECKSUMMED).await;
    assert_eq!(body["risk"], "banned");
    assert_eq!(body["blocked"], true);
    // The echoed address is the normalized one, not what was sent.
    assert_eq!(body["address"], EVM_LOWER);
    assert_eq!(body["matches"].as_array().unwrap().len(), 1);
    assert_eq!(body["matches"][0]["source"], "ofac_sdn");
    assert_eq!(body["matches"][0]["reason"], "SDN");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screen_unlisted_address_is_none_and_not_blocked() {
    let (pool, _guard) = fresh_pool().await;
    let base = spawn(&pool).await;

    let body = screen_one(&base, "evm", EVM_CLEAN).await;
    assert_eq!(body["risk"], "none");
    assert_eq!(body["blocked"], false);
    assert_eq!(body["matches"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screen_takes_max_risk_across_sources() {
    let (pool, _guard) = fresh_pool().await;
    insert_entry(&pool, "evm", EVM_LOWER, "high", "internal", None).await;
    insert_entry(&pool, "evm", EVM_LOWER, "banned", "ofac_sdn", None).await;
    let base = spawn(&pool).await;

    let body = screen_one(&base, "evm", EVM_LOWER).await;
    assert_eq!(body["risk"], "banned");
    assert_eq!(body["matches"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screen_batch_returns_verdicts_in_request_order() {
    let (pool, _guard) = fresh_pool().await;
    insert_entry(&pool, "evm", EVM_LOWER, "banned", "ofac_sdn", None).await;
    let base = spawn(&pool).await;

    let body: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/v1/screen/batch"))
        .json(&serde_json::json!({
            "chain": "evm",
            "addresses": [EVM_CLEAN, EVM_CHECKSUMMED, EVM_CLEAN],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let out = body.as_array().unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(out[0]["risk"], "none");
    assert_eq!(out[1]["risk"], "banned");
    assert_eq!(out[1]["address"], EVM_LOWER);
    assert_eq!(out[2]["risk"], "none");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_evm_address_is_rejected() {
    let (pool, _guard) = fresh_pool().await;
    let base = spawn(&pool).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/screen"))
        .json(&serde_json::json!({"chain": "evm", "address": "nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_evm_chain_is_case_sensitive() {
    let (pool, _guard) = fresh_pool().await;
    let btc = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
    insert_entry(&pool, "btc", btc, "banned", "ofac_sdn", None).await;
    let base = spawn(&pool).await;

    let hit = screen_one(&base, "btc", btc).await;
    assert_eq!(hit["risk"], "banned");

    // Lowercasing a base58 address yields a different address and must not match,
    // confirming the EVM case-folding is not applied to every chain.
    let miss = screen_one(&base, "btc", &btc.to_lowercase()).await;
    assert_eq!(miss["risk"], "none");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_entries_filters_by_chain_and_source() {
    let (pool, _guard) = fresh_pool().await;
    insert_entry(&pool, "evm", EVM_LOWER, "banned", "ofac_sdn", None).await;
    insert_entry(&pool, "evm", EVM_CLEAN, "low", "internal", None).await;
    insert_entry(&pool, "btc", "1abc", "high", "ofac_sdn", None).await;
    let base = spawn(&pool).await;

    let body: serde_json::Value =
        reqwest::get(format!("{base}/v1/entries?chain=evm&source=ofac_sdn"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["address"], EVM_LOWER);
    assert_eq!(rows[0]["risk"], "banned");
}

// ───────────────────────────────────────────────────────────── harness ──

/// Cleared between tests.
const TABLES: &[&str] = &["screened_addresses"];

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    test_support::fresh_pool(database::PoolCfg::relayer(), TABLES).await
}

async fn insert_entry(
    pool: &database::DbPool,
    chain: &str,
    address: &str,
    risk: &str,
    source: &str,
    reason: Option<&str>,
) {
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(screened_addresses::table)
        .values((
            screened_addresses::chain.eq(chain),
            screened_addresses::address.eq(address),
            screened_addresses::risk.eq(risk),
            screened_addresses::source.eq(source),
            screened_addresses::reason.eq(reason),
        ))
        .execute(&mut conn)
        .await
        .unwrap();
}

/// Boot the real router in-process. Each test gets its own service, so the verdict
/// cache cannot leak a fixture across tests.
async fn spawn(pool: &database::DbPool) -> String {
    let cfg = Arc::new(RiskWebserverConfig {
        database_url: String::new(),
        bind_addr: String::new(),
        cache_ttl_s: 60,
    });
    let state = build_state(cfg, pool.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = build_router(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn screen_one(base: &str, chain: &str, address: &str) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("{base}/v1/screen"))
        .json(&serde_json::json!({"chain": chain, "address": address}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
