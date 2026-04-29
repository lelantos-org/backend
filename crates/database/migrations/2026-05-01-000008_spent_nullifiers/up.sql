-- Per-nf burn log fed by the MASP `NullifierConsumed` event.
--
-- Wallets reconcile cached notes against this set on sync (one batch
-- HTTP roundtrip via fmd-webserver `/v1/spent`) instead of N per-nf
-- `eth_call`s to `MASP.spent(bytes32)`.
--
-- Reorg-safe: PK on log coords matches `tree_advances` / `asset_flows`
-- convention, so reorg cleanup is a single
-- `DELETE … WHERE block_number > $cursor`. `UNIQUE (chain_id, nf)`
-- guards against duplicate emits (impossible on honest chain, but the
-- DB enforces logical uniqueness anyway).

CREATE TABLE spent_nullifiers (
    chain_id     BIGINT  NOT NULL,
    block_number BIGINT  NOT NULL,
    log_index    INTEGER NOT NULL,
    nf           BYTEA   NOT NULL,
    tx_hash      BYTEA   NOT NULL,
    block_ts     BIGINT  NOT NULL,
    PRIMARY KEY (chain_id, block_number, log_index),
    UNIQUE (chain_id, nf)
);
