-- Rows written against the `pulled` pool bind the cap into their digest, so
-- they cannot be flushed without it. They are left in place all the same, as
-- the up-migration leaves the old pool's: the binary this rolls back to cannot
-- flush that pool whatever the table holds.
ALTER TABLE deposit_escrowed_events DROP COLUMN pulled;
