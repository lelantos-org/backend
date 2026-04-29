CREATE TABLE assets (
    chain_id      BIGINT NOT NULL,
    asset_id_u64  BIGINT NOT NULL,
    token         BYTEA  NOT NULL,
    scale         NUMERIC(78, 0) NOT NULL,
    gen_x         NUMERIC(78, 0) NOT NULL,
    gen_y         NUMERIC(78, 0) NOT NULL,
    PRIMARY KEY (chain_id, asset_id_u64)
);
CREATE INDEX assets_token_idx ON assets (chain_id, token);

CREATE TABLE transactions (
    chain_id         BIGINT  NOT NULL,
    tx_hash          BYTEA   NOT NULL,
    block_number     BIGINT  NOT NULL,
    block_ts         BIGINT  NOT NULL,
    merkle_root      BYTEA   NOT NULL,
    caller           BYTEA   NOT NULL,
    public_asset_id  BIGINT  NOT NULL,
    public_in_u64    BIGINT  NOT NULL,
    public_out_u64   BIGINT  NOT NULL,
    recipient        BYTEA   NOT NULL,
    nullifiers       BYTEA[] NOT NULL DEFAULT '{}',
    out_cms          BYTEA[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (chain_id, tx_hash)
);
CREATE INDEX transactions_chain_block_idx ON transactions (chain_id, block_number);
CREATE INDEX transactions_asset_idx       ON transactions (chain_id, public_asset_id);

-- Daily flow keyed by asset_id (the registry id, not token address) so it
-- still works before the indexer has seen the matching AssetRegistered.
CREATE TABLE asset_flow_daily (
    chain_id        BIGINT NOT NULL,
    asset_id_u64    BIGINT NOT NULL,
    day             DATE   NOT NULL,
    public_in_u64   NUMERIC(78, 0) NOT NULL DEFAULT 0,
    public_out_u64  NUMERIC(78, 0) NOT NULL DEFAULT 0,
    deposit_count   BIGINT NOT NULL DEFAULT 0,
    withdraw_count  BIGINT NOT NULL DEFAULT 0,
    transfer_count  BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (chain_id, asset_id_u64, day)
);
