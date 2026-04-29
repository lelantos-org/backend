-- Hourly aggregate of tree_advances for explorer dashboards.
--
-- Read by explorer-webserver for tx-counts and 24h chain flows. Refreshed
-- by explorer-indexer after each tick that committed RootAdvanced rows.
-- Hourly grain is the lowest common denominator for the UI's bucket sizes
-- (3600, 21600, 86400) — see explorer-ui/src/lib/ranges.ts.

CREATE MATERIALIZED VIEW tree_advances_hourly AS
SELECT
    chain_id,
    (block_ts / 3600) * 3600 AS ts_hour,
    SUM(inserted)::BIGINT     AS tx_count
FROM tree_advances
GROUP BY chain_id, ts_hour
WITH NO DATA;

-- Unique index required for REFRESH MATERIALIZED VIEW CONCURRENTLY.
CREATE UNIQUE INDEX tree_advances_hourly_pk
    ON tree_advances_hourly (chain_id, ts_hour);
CREATE INDEX tree_advances_hourly_ts_idx
    ON tree_advances_hourly (ts_hour);

REFRESH MATERIALIZED VIEW tree_advances_hourly;
