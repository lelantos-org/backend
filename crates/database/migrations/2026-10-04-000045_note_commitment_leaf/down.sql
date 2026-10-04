-- The dropped value commitments are not recoverable. Existing rows read the
-- identity point `(0, 1)`, which is wrong for them and harmless: no flush can
-- act on a row of the hash-bound pool under the old layout. The default only
-- fills the existing rows and is dropped again, as in 000044.
ALTER TABLE deposit_escrowed_events RENAME COLUMN fee_inner TO fee_cm;
ALTER TABLE deposit_escrowed_events RENAME COLUMN "inner" TO cm;

ALTER TABLE deposit_escrowed_events
    ADD COLUMN cv_dep_x     NUMERIC NOT NULL DEFAULT 0,
    ADD COLUMN cv_dep_y     NUMERIC NOT NULL DEFAULT 1,
    ADD COLUMN fee_cv_dep_x NUMERIC NOT NULL DEFAULT 0,
    ADD COLUMN fee_cv_dep_y NUMERIC NOT NULL DEFAULT 1;

ALTER TABLE deposit_escrowed_events
    ALTER COLUMN cv_dep_x     DROP DEFAULT,
    ALTER COLUMN cv_dep_y     DROP DEFAULT,
    ALTER COLUMN fee_cv_dep_x DROP DEFAULT,
    ALTER COLUMN fee_cv_dep_y DROP DEFAULT;

ALTER TABLE notes
    ADD COLUMN cv_dep_x NUMERIC NOT NULL DEFAULT 0,
    ADD COLUMN cv_dep_y NUMERIC NOT NULL DEFAULT 1;

ALTER TABLE notes
    ALTER COLUMN cv_dep_x DROP DEFAULT,
    ALTER COLUMN cv_dep_y DROP DEFAULT;

DROP INDEX IF EXISTS notes_chain_leaf_idx;
CREATE UNIQUE INDEX notes_chain_leaf_idx
    ON notes (chain_id, leaf_index) INCLUDE (cm, cv_dep_x, cv_dep_y);
