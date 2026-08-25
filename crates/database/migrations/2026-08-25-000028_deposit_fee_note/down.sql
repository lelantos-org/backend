-- Dropping the fee note leaves rows that cannot be flushed against the
-- two-leaf pool, so clear them as the up-migration does.
DELETE FROM deposit_escrowed_events;

ALTER TABLE deposit_escrowed_events
    DROP COLUMN fee_in,
    DROP COLUMN fee_cm,
    DROP COLUMN fee_cv_dep_x,
    DROP COLUMN fee_cv_dep_y,
    DROP COLUMN fee_rcv,
    DROP COLUMN fee_aux;
