-- All-time per-(chain, asset) totals of what the pool escrows.
--
-- Read by explorer-webserver for "locked by chain"; refreshed by
-- explorer-indexer after each tick that committed AssetMoved rows, alongside
-- asset_flows_hourly.
--
-- Deliberately NOT a net column: `in_base` and `out_base` are kept apart so the
-- caller subtracts in whatever unit it converts to, and so a negative result is
-- traceable to its two halves. Base units, per asset, exactly as
-- asset_flows_hourly stores them — there is no cross-asset total here, because
-- base units of different tokens are not addable. USD is the only aggregate a
-- caller can build from these rows, and only after dividing each asset by its
-- own decimals.
--
-- Rolling this up from asset_flows_hourly instead would sum an already-summed
-- NUMERIC per hour with no gain: the row count is the same order and the source
-- table is the authority.

CREATE MATERIALIZED VIEW asset_locked AS
SELECT
    chain_id,
    asset_id_u64,
    token,
    SUM(in_amount)::NUMERIC(78, 0)  AS in_base,
    SUM(out_amount)::NUMERIC(78, 0) AS out_base,
    MAX(block_ts)                   AS last_ts
FROM asset_flows
GROUP BY chain_id, asset_id_u64, token
WITH NO DATA;

-- Unique index required for REFRESH MATERIALIZED VIEW CONCURRENTLY.
CREATE UNIQUE INDEX asset_locked_pk
    ON asset_locked (chain_id, asset_id_u64);
CREATE INDEX asset_locked_chain_idx
    ON asset_locked (chain_id);

REFRESH MATERIALIZED VIEW asset_locked;
