-- Rows written against the fee-asset pool bind `feeAssetId` into their digest,
-- so they cannot be flushed without it; clear them as the up-migration does.
DELETE FROM deposit_escrowed_events;

ALTER TABLE deposit_escrowed_events DROP COLUMN fee_asset_id;
