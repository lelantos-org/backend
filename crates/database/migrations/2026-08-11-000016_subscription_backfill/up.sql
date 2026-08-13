-- Per-subscription backfill pointer.
--
-- Registering a subscription used to rewind the shared `fmd-filter` cursor to
-- 0 across all chains, so every POST /v1/subscriptions forced the indexer to
-- re-scan all history against the *entire* subscriber set. Unauthenticated,
-- that is a DoS amplifier: N registrations cost N full cartesian rescans.
--
-- Instead each subscription carries how far back it has been caught up.
-- `0` means "not yet scanned against history"; the filter worker walks one
-- lagging subscription forward by one batch per tick until the pointer
-- reaches the shared cursor. New notes are still covered by the main loop,
-- so the two passes together cover every (note, subscription) pair. The
-- overlap can re-insert a match, which `ON CONFLICT DO NOTHING` absorbs.

ALTER TABLE subscriptions
  ADD COLUMN backfilled_through_note_id BIGINT NOT NULL DEFAULT 0;

-- Existing subscriptions were registered under the rewind behaviour, so they
-- have already been scanned against all history. Mark them complete rather
-- than forcing a fleet-wide backfill on deploy.
UPDATE subscriptions
SET backfilled_through_note_id = COALESCE(
    (SELECT MAX(last_event_id) FROM consumer_cursors WHERE name = 'fmd-filter'),
    0
);

CREATE INDEX subscriptions_backfill_idx
    ON subscriptions (backfilled_through_note_id)
    WHERE active;
