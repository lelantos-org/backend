-- Drop the per-asset Baby-Jubjub generator columns. The generator is now
-- derived in-circuit from `asset_id_u64` via `HashToAssetGen`, so the
-- value carries no information beyond the asset id and the registry no
-- longer stores it on-chain (see contracts AssetRegistry change).
ALTER TABLE assets DROP COLUMN gen_x;
ALTER TABLE assets DROP COLUMN gen_y;
