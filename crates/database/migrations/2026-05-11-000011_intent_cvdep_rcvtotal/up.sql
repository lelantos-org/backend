-- Carry depositor-side Pedersen value commitments + total blinding sum from
-- IntentEscrowed event into the mempool. Required by the relayer's
-- TreeUpdateBatch witness gen + per-pair deposit binding aggregate after
-- the C-1 fix landed in tree_update_batch.circom (cv_dep, rcv_total).
-- All four cv_dep coords are BN254 field elements (≤ 2^254), rcv_total is
-- a scalar; NUMERIC(78,0) covers any u256.

ALTER TABLE intent_escrowed_events
    ADD COLUMN cv_dep0_x  NUMERIC(78, 0) NOT NULL DEFAULT 0,
    ADD COLUMN cv_dep0_y  NUMERIC(78, 0) NOT NULL DEFAULT 0,
    ADD COLUMN cv_dep1_x  NUMERIC(78, 0) NOT NULL DEFAULT 0,
    ADD COLUMN cv_dep1_y  NUMERIC(78, 0) NOT NULL DEFAULT 0,
    ADD COLUMN rcv_total  NUMERIC(78, 0) NOT NULL DEFAULT 0;

ALTER TABLE intent_escrowed_events
    ALTER COLUMN cv_dep0_x DROP DEFAULT,
    ALTER COLUMN cv_dep0_y DROP DEFAULT,
    ALTER COLUMN cv_dep1_x DROP DEFAULT,
    ALTER COLUMN cv_dep1_y DROP DEFAULT,
    ALTER COLUMN rcv_total DROP DEFAULT;
