-- Every deposit now mints two leaves: the depositor's note and a note paying
-- whoever flushes the batch. The second leaf's fields are digest preimage —
-- `MASP._depositDigest` binds `feeIn`, `feeCm` and `feeCvDep` — so a relayer
-- that cannot read them back exactly cannot flush the deposit at all.
--
-- OPERATIONAL NOTE: MASP is immutable, so this ships with a new pool
-- deployment. Rows for the old pool describe one-leaf deposits and have no
-- fee note to backfill; they are deleted rather than defaulted, because a
-- zero fee note is a *valid* deposit shape and defaulting would make stale
-- rows indistinguishable from real zero-fee ones. Re-ingest from the new
-- deployment.

DELETE FROM deposit_escrowed_events;

ALTER TABLE deposit_escrowed_events
    ADD COLUMN fee_in       NUMERIC NOT NULL,
    ADD COLUMN fee_cm       BYTEA   NOT NULL,
    ADD COLUMN fee_cv_dep_x NUMERIC NOT NULL,
    ADD COLUMN fee_cv_dep_y NUMERIC NOT NULL,
    ADD COLUMN fee_rcv      NUMERIC NOT NULL,
    ADD COLUMN fee_aux      JSONB   NOT NULL;
