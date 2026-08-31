-- Restore the non-covering index from migration 32.
CREATE INDEX IF NOT EXISTS asset_flows_public_out_idx
    ON asset_flows (chain_id, asset_id_u64, public_out)
    WHERE public_out IS NOT NULL AND public_out > 0;

DROP INDEX IF EXISTS asset_flows_public_out_covering_idx;
