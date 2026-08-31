-- Make the denomination-cohort aggregate index-only.
--
-- `asset_flows_public_out_idx` covers the grouping columns but not `block_ts`,
-- and the cohort query reads `block_ts` three times per row: `MIN`, `MAX`, and
-- the `FILTER` behind the recency count. So every row it grouped needed a heap
-- fetch purely to read one column the index could have carried.
--
-- Measured on a synthetic 600k-withdrawal dataset (3 chains, 3 assets, 30k
-- distinct denominations, `LIMIT 1000`):
--
--     before   Index Scan, 89,161 buffers, 63.1 ms warm
--     after    Index Only Scan, 1,155 buffers, 19.5 ms warm, Heap Fetches: 0
--
-- 77x fewer buffers, 3.2x faster. The index grows 24 MB -> 29 MB at that row
-- count, which is the whole cost: one extra non-key column, stored only in leaf
-- pages, so it does not widen the tree or change what the index can be searched
-- by.
--
-- Index-only scans depend on the visibility map, so a table mid-write still
-- takes some heap fetches until autovacuum catches up. That degrades toward the
-- old behaviour rather than below it.
--
-- On a deployment where `asset_flows` is already large, build it out of band
-- first, as migration 29 describes:
--
--     CREATE INDEX CONCURRENTLY asset_flows_public_out_covering_idx ...
--
-- and this migration then no-ops on the create. It runs at process startup
-- behind the migration advisory lock, and a standby gives up on that lock after
-- 120s, so a long in-migration build would fail the deploy rather than block.
CREATE INDEX IF NOT EXISTS asset_flows_public_out_covering_idx
    ON asset_flows (chain_id, asset_id_u64, public_out)
    INCLUDE (block_ts)
    WHERE public_out IS NOT NULL AND public_out > 0;

-- Fully superseded: same leading columns, same predicate, strictly more payload.
-- With both present the planner picks the covering one, so keeping the old
-- index only costs write throughput and space.
DROP INDEX IF EXISTS asset_flows_public_out_idx;
