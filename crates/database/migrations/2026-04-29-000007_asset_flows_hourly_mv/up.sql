-- Hourly aggregate of asset_flows for explorer dashboards.
--
-- Read by explorer-webserver for per-(chain, asset) inflow/outflow charts.
-- Refreshed by explorer-indexer after each tick that committed AssetMoved
-- rows. Hourly grain matches tree_advances_hourly so all UI bucket sizes
-- (3600, 21600, 86400) roll up cleanly.

CREATE MATERIALIZED VIEW asset_flows_hourly AS
SELECT
    chain_id,
    asset_id_u64,
    token,
    (block_ts / 3600) * 3600       AS ts_hour,
    SUM(in_amount)::NUMERIC(78, 0)  AS in_amount,
    SUM(out_amount)::NUMERIC(78, 0) AS out_amount
FROM asset_flows
GROUP BY chain_id, asset_id_u64, token, ts_hour
WITH NO DATA;

CREATE UNIQUE INDEX asset_flows_hourly_pk
    ON asset_flows_hourly (chain_id, asset_id_u64, ts_hour);
CREATE INDEX asset_flows_hourly_chain_ts_idx
    ON asset_flows_hourly (chain_id, ts_hour);
CREATE INDEX asset_flows_hourly_ts_idx
    ON asset_flows_hourly (ts_hour);

REFRESH MATERIALIZED VIEW asset_flows_hourly;
