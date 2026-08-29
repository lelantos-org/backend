DROP INDEX IF EXISTS asset_flows_public_out_idx;
ALTER TABLE asset_flows DROP COLUMN public_out;
ALTER TABLE asset_flows DROP COLUMN public_in;
