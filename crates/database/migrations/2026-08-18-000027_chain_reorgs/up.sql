-- Durable log of every reorg rewind the ingester applied.
--
-- `pg_notify` alone is not enough: it is fire-and-forget, so a consumer that
-- is down when the reorg happens never learns about it. Consumers stream
-- `raw_events` by ascending `id` and therefore re-read the replacement rows on
-- their own, but state they already derived from the *deleted* rows sits below
-- their cursor and is invisible to it. Without an explicit record, that state
-- is never retracted.
--
-- Written inside the same transaction as the `raw_events` delete, so a rewind
-- and its marker cannot come apart.
CREATE TABLE chain_reorgs (
    id         BIGSERIAL PRIMARY KEY,
    chain_id   BIGINT      NOT NULL,
    -- First block discarded. Everything from here up was re-derived.
    rewind_to  BIGINT      NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX chain_reorgs_chain_idx ON chain_reorgs (chain_id, id);

-- How far each consumer has processed the reorg log. Without this a consumer
-- would re-apply the same retraction on every tick and never make progress.
ALTER TABLE consumer_cursors ADD COLUMN last_reorg_id BIGINT NOT NULL DEFAULT 0;
