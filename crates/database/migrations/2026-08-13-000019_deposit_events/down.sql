-- Reverse the single-leaf rename. The second-leaf columns come back empty:
-- the data they held was dropped going up and cannot be reconstructed.

ALTER INDEX deposit_pending_idx RENAME TO intent_pending_idx;

ALTER TABLE deposit_escrowed_events
    RENAME CONSTRAINT deposit_escrowed_events_chain_id_deposit_id_key
    TO intent_escrowed_events_chain_id_intent_id_key;
ALTER TABLE deposit_escrowed_events
    RENAME CONSTRAINT deposit_escrowed_events_pkey TO intent_escrowed_events_pkey;

ALTER TABLE deposit_escrowed_events
    ADD COLUMN cm1       BYTEA NOT NULL DEFAULT '\x'::bytea,
    ADD COLUMN cv_dep1_x NUMERIC(78, 0) NOT NULL DEFAULT 0,
    ADD COLUMN cv_dep1_y NUMERIC(78, 0) NOT NULL DEFAULT 0;

ALTER TABLE deposit_escrowed_events
    ALTER COLUMN cm1 DROP DEFAULT,
    ALTER COLUMN cv_dep1_x DROP DEFAULT,
    ALTER COLUMN cv_dep1_y DROP DEFAULT;

ALTER TABLE deposit_escrowed_events RENAME COLUMN rcv      TO rcv_total;
ALTER TABLE deposit_escrowed_events RENAME COLUMN cv_dep_y TO cv_dep0_y;
ALTER TABLE deposit_escrowed_events RENAME COLUMN cv_dep_x TO cv_dep0_x;
ALTER TABLE deposit_escrowed_events RENAME COLUMN cm       TO cm0;
ALTER TABLE deposit_escrowed_events RENAME COLUMN deposit_id TO intent_id;

ALTER TABLE deposit_escrowed_events RENAME TO intent_escrowed_events;
