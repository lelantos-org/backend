-- The dropped blinders are not recoverable. Rows written since the up-migration
-- are escrows of the fixed-blinder pool, so they are backfilled with 1; rows
-- from before it read 1 as well, which is wrong for them and harmless, as the
-- up-migration explains: no flush can act on them. The default only fills the
-- existing rows and is dropped again, as in 000011.
ALTER TABLE deposit_escrowed_events
    ADD COLUMN rcv     NUMERIC NOT NULL DEFAULT 1,
    ADD COLUMN fee_rcv NUMERIC NOT NULL DEFAULT 1;

ALTER TABLE deposit_escrowed_events
    ALTER COLUMN rcv     DROP DEFAULT,
    ALTER COLUMN fee_rcv DROP DEFAULT;
