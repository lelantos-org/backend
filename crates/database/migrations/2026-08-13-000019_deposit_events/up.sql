-- MASP moved from the two-leaf `Intent*` escrow flow to the single-leaf
-- `Deposit*` flow: `IntentEscrowed`/`IntentFlushed`/`IntentCanceled` became
-- `DepositEscrowed`/`DepositFlushed`/`DepositCanceled`, and a deposit now
-- occupies exactly one leaf. The event carries one `cm`, one `cvDep` and the
-- per-leaf blinder `rcv` (was `rcvTotal` over two leaves).
--
-- OPERATIONAL NOTE: the on-chain log layout changed, so every previously
-- ingested row for the old pool is undecodable. This migration drops the rows
-- of this table because a two-leaf escrow cannot be mapped onto a one-leaf
-- one; the rest of the pipeline (raw_events, notes, consumer_cursors) must be
-- re-ingested from the new deployment separately.

ALTER TABLE intent_escrowed_events RENAME TO deposit_escrowed_events;

ALTER TABLE deposit_escrowed_events RENAME COLUMN intent_id TO deposit_id;
ALTER TABLE deposit_escrowed_events RENAME COLUMN cm0        TO cm;
ALTER TABLE deposit_escrowed_events RENAME COLUMN cv_dep0_x  TO cv_dep_x;
ALTER TABLE deposit_escrowed_events RENAME COLUMN cv_dep0_y  TO cv_dep_y;
ALTER TABLE deposit_escrowed_events RENAME COLUMN rcv_total  TO rcv;

ALTER TABLE deposit_escrowed_events
    DROP COLUMN cm1,
    DROP COLUMN cv_dep1_x,
    DROP COLUMN cv_dep1_y;

ALTER TABLE deposit_escrowed_events
    RENAME CONSTRAINT intent_escrowed_events_pkey TO deposit_escrowed_events_pkey;
ALTER TABLE deposit_escrowed_events
    RENAME CONSTRAINT intent_escrowed_events_chain_id_intent_id_key
    TO deposit_escrowed_events_chain_id_deposit_id_key;

ALTER INDEX intent_pending_idx RENAME TO deposit_pending_idx;

DELETE FROM deposit_escrowed_events;
