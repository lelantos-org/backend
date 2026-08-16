DROP INDEX assets_metadata_missing_idx;
CREATE INDEX assets_decimals_missing_idx ON assets (chain_id) WHERE decimals IS NULL;
ALTER TABLE assets DROP COLUMN symbol;
