DROP INDEX IF EXISTS subscriptions_backfill_idx;
ALTER TABLE subscriptions DROP COLUMN backfilled_through_note_id;
