-- Classify transactions per operation rather than per transaction hash.
--
-- The `Bundler` contract lands several MASP operations in one transaction — any
-- mix of `flushBatch`, `transfer`, `withdraw`, `withdrawNative` and `swap` — so a
-- tx hash no longer names one kind. Each operation owns exactly one
-- `RootAdvanced`, and the classified feed in explorer-webserver's
-- `repositories::transactions` now scopes its anti-joins to that operation's
-- own log range instead of the whole transaction:
--
-- - a flush's `DepositFlushed` logs sit after the previous `RootAdvanced` in the
--   transaction and before its own;
-- - a withdrawal's `AssetMoved` sits after its `RootAdvanced` and before the
--   next one.
--
-- `asset_flows` already stores its log index. `deposit_escrowed_events` recorded
-- the flushing transaction but not where in it the flush sat, which is what
-- this column adds.
ALTER TABLE deposit_escrowed_events ADD COLUMN flushed_log_index INTEGER;

-- Backfill from the raw store: the `DepositFlushed` in the flushing transaction
-- whose indexed id (`topics[2]`) matches the id of this row's own
-- `DepositEscrowed`. Served by `raw_events_chain_tx_idx (chain_id, tx_hash)` and
-- the raw store's unique (chain_id, block_number, log_index).
UPDATE deposit_escrowed_events d
   SET flushed_log_index = f.log_index
  FROM raw_events e
  JOIN raw_events f
    ON f.chain_id = e.chain_id
   AND f.event_kind = 7  -- EventKind::DepositFlushed
   AND f.topics[2] = e.topics[2]
 WHERE d.flushed_tx_hash IS NOT NULL
   AND e.chain_id = d.chain_id
   AND e.block_number = d.block_number
   AND e.log_index = d.log_index
   AND f.tx_hash = d.flushed_tx_hash;

-- The per-operation range lookup: every `RootAdvanced` sharing a transaction,
-- ordered by position. No index on `tree_advances` carried `tx_hash` at all.
--
-- On a deployment where `tree_advances` is already large, build it out of band
-- first, as migrations 29 and 38 describe:
--
--     CREATE INDEX CONCURRENTLY tree_advances_chain_tx_idx ON tree_advances (chain_id, tx_hash, log_index);
--
-- and this migration then no-ops on it.
CREATE INDEX IF NOT EXISTS tree_advances_chain_tx_idx
    ON tree_advances (chain_id, tx_hash, log_index);
