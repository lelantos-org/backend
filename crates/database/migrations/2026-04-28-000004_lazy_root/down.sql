-- Reverse of 0004_lazy_root/up.sql. Restores the v1 shape but does NOT
-- back-fill any data — caller is responsible for rebuilding from chain.

DROP TABLE IF EXISTS tree_advances;

DROP INDEX IF EXISTS notes_chain_leaf_idx;
ALTER TABLE notes DROP COLUMN leaf_index;
ALTER TABLE notes ADD COLUMN clue_bits_u16 INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notes ALTER COLUMN clue_bits_u16 DROP DEFAULT;

CREATE TABLE nullifiers (
    id           BIGSERIAL PRIMARY KEY,
    chain_id     BIGINT NOT NULL,
    block_number BIGINT NOT NULL,
    tx_hash      BYTEA  NOT NULL,
    nullifier    BYTEA  NOT NULL,
    UNIQUE (chain_id, nullifier)
);
CREATE INDEX nullifiers_chain_block_idx ON nullifiers (chain_id, block_number);

CREATE TABLE transactions (
    chain_id          BIGINT  NOT NULL,
    tx_hash           BYTEA   NOT NULL,
    block_number      BIGINT  NOT NULL,
    block_ts          BIGINT  NOT NULL,
    merkle_root       BYTEA   NOT NULL,
    caller            BYTEA   NOT NULL,
    public_asset_id   BIGINT  NOT NULL,
    public_in_u64     BIGINT  NOT NULL,
    public_out_u64    BIGINT  NOT NULL,
    recipient         BYTEA   NOT NULL,
    nullifiers        BYTEA[] NOT NULL,
    out_cms           BYTEA[] NOT NULL,
    PRIMARY KEY (chain_id, tx_hash)
);

CREATE TABLE asset_flow_daily (
    chain_id        BIGINT NOT NULL,
    asset_id_u64    BIGINT NOT NULL,
    day             DATE   NOT NULL,
    public_in_u64   NUMERIC(78, 0) NOT NULL,
    public_out_u64  NUMERIC(78, 0) NOT NULL,
    deposit_count   BIGINT NOT NULL,
    withdraw_count  BIGINT NOT NULL,
    transfer_count  BIGINT NOT NULL,
    PRIMARY KEY (chain_id, asset_id_u64, day)
);
