//! Migration 000043, `deposit_escrowed_events.pulled`, applied to a table that
//! already holds deposits.
//!
//! The column ships with a fresh pool, so no row needs a real backfill. But the
//! table is not empty where the migration runs: three binaries apply it at
//! startup, against whatever the previous pool left behind, and a `NOT NULL`
//! column cannot be added to those rows without saying what they get. Every
//! other test starts from a template database migrated while it was empty,
//! which is the one case that cannot show this.
//!
//! One test, and a file of its own: it rewinds the schema of the database every
//! test in its binary shares.

use bigdecimal::BigDecimal;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Numeric};
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};

const UP: &str = include_str!("../../database/migrations/2026-10-03-000043_deposit_pulled/up.sql");
const DOWN: &str =
    include_str!("../../database/migrations/2026-10-03-000043_deposit_pulled/down.sql");
/// Every migration after 000043, newest first. The shared database is fully
/// migrated, so rewinding to the schema 000043 starts from goes through these.
const LATER_DOWN: [&str; 3] = [
    include_str!("../../database/migrations/2026-10-04-000046_notes_repeated_cm/down.sql"),
    include_str!("../../database/migrations/2026-10-04-000045_note_commitment_leaf/down.sql"),
    include_str!("../../database/migrations/2026-10-03-000044_deposit_drop_rcv/down.sql"),
];

/// A `uint256` at full width, as a `DepositEscrowed` log can carry.
const UINT256_MAX: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

/// One deposit as the schema before this migration took it: every required
/// column, and no `pulled`.
async fn insert_without_pulled(conn: &mut AsyncPgConnection, id: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO deposit_escrowed_events (chain_id, block_number, log_index, deposit_id, \
           payer, recipient, public_asset_id, public_in, fee_bps_at_submit, cm, cv_dep_x, \
           cv_dep_y, rcv, aux, fee_asset_id, fee_in, fee_cm, fee_cv_dep_x, fee_cv_dep_y, \
           fee_rcv, fee_aux, submitted_at_block, tx_hash, block_ts) \
         VALUES (1, $1, 0, $1, '\\x01', '\\x02', 7, 100, 25, '\\x03', 0, 0, 0, '{}', 0, 0, \
           '\\x04', 0, 0, 0, '{}', $1, '\\x05', 0)",
    )
    .bind::<BigInt, _>(id)
    .execute(conn)
    .await
}

#[derive(QueryableByName)]
struct Cap {
    #[diesel(sql_type = BigInt)]
    block_number: i64,
    #[diesel(sql_type = Numeric)]
    pulled: BigDecimal,
}

/// `(block_number, pulled)` of every row, oldest first.
async fn caps(conn: &mut AsyncPgConnection) -> Vec<(i64, BigDecimal)> {
    diesel::sql_query(
        "SELECT block_number, pulled FROM deposit_escrowed_events ORDER BY block_number",
    )
    .load::<Cap>(conn)
    .await
    .unwrap()
    .into_iter()
    .map(|c| (c.block_number, c.pulled))
    .collect()
}

#[tokio::test]
async fn pulled_is_added_to_a_populated_table_and_keeps_no_default() {
    let (pool, _guard) =
        test_support::fresh_pool(database::PoolCfg::indexer(), &["deposit_escrowed_events"]).await;
    let mut conn = pool.get().await.unwrap();

    // The schema the migration starts from, holding the previous pool's deposits.
    for later in LATER_DOWN {
        conn.batch_execute(later)
            .await
            .expect("a later migration's down applies");
    }
    conn.batch_execute(DOWN).await.expect("down applies");
    for id in [1, 2] {
        insert_without_pulled(&mut conn, id)
            .await
            .expect("the previous schema takes a row with no `pulled`");
    }

    conn.batch_execute(UP)
        .await
        .expect("up applies to a populated table");

    // Kept rather than deleted, and readable through the `NOT NULL` column.
    let zero = BigDecimal::from(0);
    assert_eq!(
        caps(&mut conn).await,
        [(1, zero.clone()), (2, zero.clone())],
        "rows from before the migration read a cap of 0"
    );

    // That 0 was a one-off fill, not a default left on the column: a writer that
    // omits the cap would otherwise store a plain asset's 0 for a yield escrow,
    // and the relayer could never flush it.
    assert!(
        insert_without_pulled(&mut conn, 3).await.is_err(),
        "an insert that omits `pulled` must be refused"
    );

    diesel::sql_query(
        "INSERT INTO deposit_escrowed_events (chain_id, block_number, log_index, deposit_id, \
           payer, recipient, public_asset_id, public_in, fee_bps_at_submit, cm, cv_dep_x, \
           cv_dep_y, rcv, aux, fee_asset_id, fee_in, fee_cm, fee_cv_dep_x, fee_cv_dep_y, \
           fee_rcv, fee_aux, pulled, submitted_at_block, tx_hash, block_ts) \
         VALUES (1, 3, 0, 3, '\\x01', '\\x02', 7, 100, 25, '\\x03', 0, 0, 0, '{}', 0, 0, \
           '\\x04', 0, 0, 0, '{}', $1, 3, '\\x05', 0)",
    )
    .bind::<Numeric, _>(UINT256_MAX.parse::<BigDecimal>().unwrap())
    .execute(&mut conn)
    .await
    .expect("the column holds a full uint256");
    assert_eq!(
        caps(&mut conn).await,
        [
            (1, zero.clone()),
            (2, zero),
            (3, UINT256_MAX.parse().unwrap())
        ]
    );
}
