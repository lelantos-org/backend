-- Per-tx token flow log for explorer dashboards.
--
-- Source: AssetMoved event emitted once per MASP `transact` whenever the
-- gross deposit/withdraw is non-zero. Internal transfers do not emit and
-- therefore do not appear here.
--
-- Amounts are token base units (already lifted by `scale`), stored as
-- NUMERIC(78, 0) to safely fit any uint256.

CREATE TABLE asset_flows (
    chain_id     BIGINT          NOT NULL,
    block_number BIGINT          NOT NULL,
    log_index    INTEGER         NOT NULL,
    asset_id_u64 BIGINT          NOT NULL,
    token        BYTEA           NOT NULL,
    in_amount    NUMERIC(78, 0)  NOT NULL,
    out_amount   NUMERIC(78, 0)  NOT NULL,
    tx_hash      BYTEA           NOT NULL,
    block_ts     BIGINT          NOT NULL,
    PRIMARY KEY (chain_id, block_number, log_index)
);

CREATE INDEX asset_flows_chain_ts_idx        ON asset_flows (chain_id, block_ts);
CREATE INDEX asset_flows_chain_asset_ts_idx  ON asset_flows (chain_id, asset_id_u64, block_ts);
