-- Per-intent escrow log fed by `IntentEscrowed` / `IntentFlushed` /
-- `IntentCanceled` events from MASP. Drives the relayer's flush mempool:
-- `flushBatch` candidates are rows where both `flushed_at_block` and
-- `canceled_at_block` are NULL, ordered by `submitted_at_block`.
--
-- Reorg-safe: PK on log coords (chain_id, block_number, log_index) matches
-- the rest of the indexer convention. `UNIQUE (chain_id, intent_id)`
-- enforces logical idempotency on the contract's monotonic id.

CREATE TABLE intent_escrowed_events (
    chain_id           BIGINT  NOT NULL,
    block_number       BIGINT  NOT NULL,
    log_index          INTEGER NOT NULL,
    intent_id          NUMERIC(78, 0) NOT NULL,
    payer              BYTEA   NOT NULL,
    recipient          BYTEA   NOT NULL,
    public_asset_id    BIGINT  NOT NULL,
    public_in          NUMERIC(20, 0) NOT NULL,
    cm0                BYTEA   NOT NULL,
    cm1                BYTEA   NOT NULL,
    aux                JSONB   NOT NULL,
    submitted_at_block BIGINT  NOT NULL,
    flushed_at_block   BIGINT,
    canceled_at_block  BIGINT,
    tx_hash            BYTEA   NOT NULL,
    block_ts           BIGINT  NOT NULL,
    PRIMARY KEY (chain_id, block_number, log_index),
    UNIQUE (chain_id, intent_id)
);

-- Pending-intent lookup index for the flush cron. Partial index keeps it
-- tight; flushed/canceled rows are immutable history.
CREATE INDEX intent_pending_idx
    ON intent_escrowed_events (chain_id, submitted_at_block)
    WHERE flushed_at_block IS NULL AND canceled_at_block IS NULL;
