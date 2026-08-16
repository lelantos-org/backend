-- ERC20 decimals per registered asset.
--
-- `assets.scale` is NOT a decimals normalizer and must not be used as one.
-- It is a circuit capacity parameter: `publicIn = baseUnits / scale` has to
-- fit `uint48`, so an 18-decimal token needs `scale >= 1e10` while a 6- or
-- 8-decimal token uses `scale = 1` (see contracts/script/Deploy.s.sol).
-- Circuit units per whole token is therefore `10^decimals / scale`, which
-- varies per asset — 1e8 for an 18-decimal token at scale 1e10, 1e6 for a
-- 6-decimal token at scale 1. Summing circuit units across assets adds unlike
-- quantities; converting to whole tokens needs `decimals`.
--
-- NULL means "not fetched yet": `AssetRegistered` does not carry decimals, so
-- the indexer reads it over RPC and backfills. Consumers must treat NULL as
-- unknown and decline to render an amount, never assume 18.
ALTER TABLE assets ADD COLUMN decimals SMALLINT;

-- Lets the indexer's backfill sweep find unfilled rows without scanning the
-- whole registry every tick.
CREATE INDEX assets_decimals_missing_idx ON assets (chain_id) WHERE decimals IS NULL;
