-- A deposit's relayer fee note may be paid in a different asset than the
-- deposit. `DepositEscrowed` gains `feeAssetId` (before `feeIn`), and
-- `MASP._depositDigest` binds it between `feeIn` and `feeCm`, so a relayer that
-- cannot read it back exactly cannot flush the deposit. It is 0 exactly when
-- `fee_in` is 0.
--
-- OPERATIONAL NOTE: this ships with a fresh pool deployment (MASP,
-- NativeAdapter, SwapWrapper); the event layout, the digest and the Permit2
-- witness all change and no compatibility path is kept. Existing rows are
-- deleted rather than defaulted: they have no fee asset to backfill, and
-- `fee_asset_id = 0` is a valid zero-fee shape, so a default would make old-pool
-- rows indistinguishable from real ones. Drain or cancel the old pool's pending
-- escrows first, then re-ingest from the new deployment (reset the indexer
-- cursors to its deploy block).
--
-- 000028's note that "MASP is immutable" is out of date: the pool sits behind
-- `DelayedUpgradeProxy`. A fresh pool is chosen here because the ABI breaks, not
-- because an upgrade is impossible.

DELETE FROM deposit_escrowed_events;

ALTER TABLE deposit_escrowed_events
    ADD COLUMN fee_asset_id BIGINT NOT NULL;
