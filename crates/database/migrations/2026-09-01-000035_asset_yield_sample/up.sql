-- A history of the pool's yield index, so a rate can be measured without an
-- archive node.
--
-- `asset_yield` holds one row per asset and is overwritten on every indexer
-- pass, so nothing anywhere remembers what the index was last week. Recovering
-- it means a historical `eth_call`, which needs archive state — and public RPCs
-- prune within hours. This table is the alternative: write down each reading as
-- it goes past, and a week later the comparison is a SELECT.
--
-- The index rather than the venue's share price. It is the figure a note is
-- actually worth, already net of the performance fee and of the idle buffer, so
-- differencing two of these gives what a holder earned rather than what the
-- venue paid less an estimate of the pool's cut.
--
-- Written by the relayer, unlike every other table here, which the indexer
-- owns. It is a derived cache and nothing reads it but the rate estimate: it
-- can be truncated at any time, costing one window of history and no
-- correctness.
CREATE TABLE asset_yield_sample (
    chain_id      BIGINT      NOT NULL,
    asset_id_u64  BIGINT      NOT NULL,
    -- When the reading was taken, from `asset_yield.updated_at` rather than the
    -- writer's clock: that is when the value was confirmed against the chain,
    -- and the elapsed time between two samples is the denominator of the rate.
    observed_at   TIMESTAMPTZ NOT NULL,
    -- RAY-scaled, copied verbatim from `asset_yield.index_ray`.
    index_ray     NUMERIC(78, 0) NOT NULL,
    -- The block the reading was confirmed at. Not used by the estimate, which
    -- measures in seconds; kept so a sample can be traced back to a chain state.
    block_number  BIGINT,

    -- One sample per asset per observation. A writer that re-reads an
    -- `asset_yield` row the indexer has not refreshed writes the same
    -- `observed_at` and is absorbed by `ON CONFLICT DO NOTHING` rather than
    -- laying down a duplicate with no new information in it.
    PRIMARY KEY (chain_id, asset_id_u64, observed_at)
);

-- The primary key already indexes `(chain_id, asset_id_u64, observed_at)`, which
-- is exactly what the estimate's query filters and orders on — a btree scans
-- either direction, so no second index on those columns would earn its keep.
--
-- This one serves the other statement: `prune` deletes by age across a whole
-- chain, and the key's index cannot give it a range on `observed_at` with
-- `asset_id_u64` sitting in between.
CREATE INDEX asset_yield_sample_age_idx
    ON asset_yield_sample (chain_id, observed_at);
