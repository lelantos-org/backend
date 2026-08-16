-- ERC20 symbol per registered asset.
--
-- `AssetRegistered` carries `(assetId, token, scale)` and no label, so every
-- client that wanted to name a token had to read `symbol()` over RPC itself —
-- once per asset, on every load. Storing it here lets the registry be served
-- complete, and removes the read from the path where its failure was silent:
-- a wallet that could not resolve a symbol fell back to `#<id>`, which also
-- made "is this the wrapped native token?" unanswerable.
--
-- NULL means "not fetched yet", exactly as for `decimals`: the indexer reads
-- it over RPC and backfills, and consumers must treat NULL as unknown rather
-- than inventing a label.
ALTER TABLE assets ADD COLUMN symbol TEXT;

-- Replaces the decimals-only index. The backfill sweeps on "either column is
-- unfilled", so the partial index has to match that predicate to stay useful;
-- two single-column partial indexes would leave the OR to a bitmap merge.
DROP INDEX assets_decimals_missing_idx;
CREATE INDEX assets_metadata_missing_idx
    ON assets (chain_id)
    WHERE decimals IS NULL OR symbol IS NULL;
