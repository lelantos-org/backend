-- A yield-asset escrow's cancellation refund is capped at what was pulled for
-- it at submit. The pool no longer stores that cap: `DepositEscrowed` gains a
-- trailing `pulled`, and `MASP._depositDigest` binds it as its last word, so
-- `flushBatch` takes it back in each `DepositMeta` and `cancelDeposit` as its
-- last argument. A flush never applies the cap, but a relayer that cannot read
-- it back exactly cannot flush the deposit. It is 0 for a plain asset.
--
-- NUMERIC(78, 0) covers any `uint256`: the cap is in the deposit token's base
-- units.
--
-- OPERATIONAL NOTE: this ships with a fresh pool deployment, like 000028 and
-- 000041: the event layout and its topic0, the digest, and the `flushBatch` and
-- cancel selectors all change, and no compatibility path is kept.
--
-- Unlike those two, existing rows are kept, and read `pulled = 0`. They are the
-- old pool's escrows, whose digest has no such word, so there is nothing to
-- backfill; and 0 is what a plain-asset escrow of the new pool stores, so on a
-- row from before this migration it says nothing about the deposit. No flush
-- can act on such a row either way: the old pool takes another `flushBatch`,
-- and in the new pool its id is empty or somebody else's.
--
-- They still have to go when the old pool is retired: the new one numbers its
-- deposits from 0 again and `(chain_id, deposit_id)` is unique, so a leftover
-- row fails the insert of the new escrow with its id. Clear this table along
-- with the rest of the old deployment's derived state when re-ingesting from
-- the new deploy block.
--
-- The default only fills the existing rows and is dropped again, as in 000011:
-- a writer that omits the cap must fail rather than store a plain asset's 0
-- for a yield escrow, which would leave the deposit unflushable.

ALTER TABLE deposit_escrowed_events
    ADD COLUMN pulled NUMERIC(78, 0) NOT NULL DEFAULT 0;

ALTER TABLE deposit_escrowed_events
    ALTER COLUMN pulled DROP DEFAULT;
