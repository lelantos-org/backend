-- Circuit-unit amounts alongside the base-unit ones on every asset flow.
--
-- `in_amount` / `out_amount` are ERC-20 base units — what actually moved.
-- `public_in` / `public_out` are the same movement as the SNARK published it.
--
-- Today the two differ only by the asset's `scale`, so `out_amount / scale`
-- recovers the circuit value and these columns are redundant. They stop being
-- redundant the moment a pool-managed yield index is live: the conversion then
-- also multiplies by an index that moves every block, and recovering the
-- circuit value off-chain would mean re-deriving contract arithmetic against a
-- figure this table does not record. Reading it from the log removes that whole
-- class of drift.
--
-- The circuit value is what withdrawal-denomination analysis groups on — an
-- anonymity set is "every withdrawal of publicOut = d", which base units cannot
-- express once the index has moved.
--
-- NULL means "observed before the contract emitted these fields", not zero. A
-- consumer aggregating denominations must skip NULL rows rather than counting
-- them as a zero-valued denomination.
ALTER TABLE asset_flows ADD COLUMN public_in NUMERIC(20, 0);
ALTER TABLE asset_flows ADD COLUMN public_out NUMERIC(20, 0);

-- Denomination anonymity sets are counted per (chain, asset, publicOut) over
-- withdrawals only, which is the access pattern this supports.
CREATE INDEX asset_flows_public_out_idx
    ON asset_flows (chain_id, asset_id_u64, public_out)
    WHERE public_out IS NOT NULL AND public_out > 0;
