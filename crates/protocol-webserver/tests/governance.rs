//! DB-backed tests for the governance routes' reads: keyset paging, tallies
//! summed in SQL, and the 404s for unknown chains and proposals.
//!
//! Rows are seeded as protocol-indexer leaves them, then read through the
//! service layer, so the assertions are on the wire JSON.

use diesel_async::RunQueryDsl;
use protocol_webserver::app::config::{ChainCfg, RegistryConfig, TokenPricesCfg};
use protocol_webserver::services::governance;
use protocol_webserver::{AppState, build_state};
use serde_json::json;
use shared::http::AppError;
use std::sync::Arc;

const CHAIN: i64 = 31337;
const TABLES: &[&str] = &["gov_proposals", "gov_votes"];

async fn state() -> (AppState, tokio::sync::OwnedMutexGuard<()>) {
    let (pool, guard) = test_support::fresh_pool(database::PoolCfg::indexer(), TABLES).await;
    let cfg = RegistryConfig {
        database_url: "unused".into(),
        bind_addr: "127.0.0.1:0".into(),
        metrics_addr: "127.0.0.1:0".into(),
        cache_ttl_s: 30,
        token_prices: TokenPricesCfg::default(),
        chains: vec![ChainCfg {
            chain_id: CHAIN,
            name: None,
            rpc_url: None,
            read_rpc_url: None,
            explorer_url: None,
            permit2_address: None,
            masp_address: None,
            tree_depth: None,
            native_adapter_address: None,
            swap_wrapper_address: None,
            governor_address: None,
            gov_token_address: None,
            timelock_address: None,
            apy_rpc_url: None,
        }],
    };
    (build_state(Arc::new(cfg), pool).unwrap(), guard)
}

async fn exec(st: &AppState, sql: &str) {
    let mut conn = st.pool.get().await.unwrap();
    diesel::sql_query(sql).execute(&mut conn).await.unwrap();
}

/// Proposal `id` created at `block`. Proposer and target are fixed so the
/// checksummed spelling is checkable.
async fn proposal(st: &AppState, id: &str, block: i64, extra: &str) {
    exec(
        st,
        &format!(
            "INSERT INTO gov_proposals (chain_id, proposal_id, proposer, targets, call_values, \
               signatures, calldatas, description, vote_start, vote_end, quorum_vote_deadline, \
               block_number, log_index, tx_hash, block_ts, queued_at_block, eta, \
               executed_at_block, canceled_at_block) \
             VALUES ({CHAIN}, {id}, '\\x5fbdb2315678afecb367f032d93f642f64180aa3', \
               ARRAY['\\x5fbdb2315678afecb367f032d93f642f64180aa3'::bytea], ARRAY[7::numeric], \
               ARRAY[''], ARRAY['\\xabcd'::bytea], E'# Proposal {id}\\n\\nBody', 1000, 1300, \
               {extra}, {block}, 0, '\\x{tx}', 0, NULL, NULL, NULL, NULL)",
            tx = "11".repeat(32),
        ),
    )
    .await;
}

async fn vote(st: &AppState, id: &str, voter_byte: u8, support: i16, weight: &str, block: i64) {
    exec(
        st,
        &format!(
            "INSERT INTO gov_votes (chain_id, proposal_id, voter, support, weight, reason, \
               params, block_number, log_index, tx_hash, block_ts) \
             VALUES ({CHAIN}, {id}, '\\x{voter}', {support}, {weight}, 'because', NULL, \
               {block}, 0, '\\x{tx}', 0)",
            voter = format!("{voter_byte:02x}").repeat(20),
            tx = "22".repeat(32),
        ),
    )
    .await;
}

const BIG_ID: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

#[tokio::test]
async fn proposals_page_newest_first_with_tallies() {
    let (st, _g) = state().await;
    proposal(&st, "1", 100, "1240").await;
    proposal(&st, "2", 101, "NULL").await;
    proposal(&st, BIG_ID, 102, "1240").await;
    vote(&st, "1", 0xa1, 1, "300", 110).await;
    vote(&st, "1", 0xa2, 1, "200", 111).await;
    vote(&st, "1", 0xa3, 0, "50", 112).await;
    vote(&st, "1", 0xa4, 2, "5", 113).await;
    exec(
        &st,
        "UPDATE gov_proposals SET queued_at_block = 120, eta = 2000, executed_at_block = 130 \
         WHERE proposal_id = 1",
    )
    .await;

    let page = governance::list_proposals(&st, CHAIN, None, Some(2))
        .await
        .unwrap();
    let json = serde_json::to_value(&page).unwrap();
    let ids: Vec<&str> = json["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["proposalId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![BIG_ID, "2"], "newest first, uint256 exact");
    assert_eq!(json["nextCursor"], "101-0");
    assert!(
        json["proposals"][1].get("quorumVoteDeadline").is_none(),
        "absent, not null"
    );

    let page = governance::list_proposals(&st, CHAIN, Some("101-0"), Some(2))
        .await
        .unwrap();
    let json = serde_json::to_value(&page).unwrap();
    assert!(json.get("nextCursor").is_none(), "last page: {json}");
    assert_eq!(
        json["proposals"][0],
        json!({
            "proposalId": "1",
            "proposer": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
            "title": "Proposal 1",
            "voteStart": 1000,
            "voteEnd": 1300,
            "quorumVoteDeadline": 1240,
            "createdBlock": 100,
            "createdTx": format!("0x{}", "11".repeat(32)),
            "queuedAtBlock": 120,
            "eta": 2000,
            "executedAtBlock": 130,
            "tallies": {"for": "500", "against": "50", "abstain": "5"},
            "voteCount": 4
        })
    );
}

#[tokio::test]
async fn detail_carries_description_and_actions() {
    let (st, _g) = state().await;
    proposal(&st, "9", 100, "1240").await;

    let got =
        serde_json::to_value(governance::get_proposal(&st, CHAIN, "9").await.unwrap()).unwrap();
    assert_eq!(got["description"], "# Proposal 9\n\nBody");
    assert_eq!(
        got["actions"],
        json!([{
            "target": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
            "value": "7",
            "signature": "",
            "calldata": "0xabcd"
        }])
    );
    assert_eq!(
        got["tallies"],
        json!({"for": "0", "against": "0", "abstain": "0"}),
        "no votes is zero, not absent"
    );
    assert_eq!(got["voteCount"], 0);
    assert_eq!(
        got["title"], "Proposal 9",
        "summary fields are flattened in"
    );
}

#[tokio::test]
async fn votes_page_newest_first() {
    let (st, _g) = state().await;
    proposal(&st, "1", 100, "NULL").await;
    for (i, block) in [110i64, 111, 112].into_iter().enumerate() {
        vote(&st, "1", 0xa0 + i as u8, 1, "10", block).await;
    }

    let page = governance::list_votes(&st, CHAIN, "1", None, Some(2))
        .await
        .unwrap();
    let json = serde_json::to_value(&page).unwrap();
    let blocks: Vec<i64> = json["votes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["blockNumber"].as_i64().unwrap())
        .collect();
    assert_eq!(blocks, vec![112, 111]);
    assert_eq!(json["nextCursor"], "111-0");
    assert_eq!(
        json["votes"][0],
        json!({
            "voter": alloy::primitives::Address::repeat_byte(0xa2).to_checksum(None),
            "support": 1,
            "weight": "10",
            "reason": "because",
            "blockNumber": 112,
            "txHash": format!("0x{}", "22".repeat(32))
        })
    );

    let page = governance::list_votes(&st, CHAIN, "1", Some("111-0"), Some(2))
        .await
        .unwrap();
    assert_eq!(page.votes.len(), 1);
    assert!(page.next_cursor.is_none());
}

#[tokio::test]
async fn unknown_chain_proposal_and_bad_input_are_rejected() {
    let (st, _g) = state().await;
    proposal(&st, "1", 100, "NULL").await;
    // An orphan vote: a proposal the indexer never saw.
    vote(&st, "77", 0xb1, 1, "10", 90).await;

    assert!(matches!(
        governance::list_proposals(&st, 1, None, None).await,
        Err(AppError::NotFound(_))
    ));
    assert!(matches!(
        governance::get_proposal(&st, CHAIN, "2").await,
        Err(AppError::NotFound(_))
    ));
    assert!(matches!(
        governance::list_votes(&st, CHAIN, "77", None, None).await,
        Err(AppError::NotFound(_))
    ));
    assert!(matches!(
        governance::get_proposal(&st, CHAIN, "0x1").await,
        Err(AppError::BadRequest(_))
    ));
    assert!(matches!(
        governance::list_proposals(&st, CHAIN, Some("nope"), None).await,
        Err(AppError::BadRequest(_))
    ));
}
