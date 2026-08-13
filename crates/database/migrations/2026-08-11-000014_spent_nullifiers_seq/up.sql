-- Dense per-chain ordinal so the spent set can be served as a chunk feed.
--
-- `/v1/chains/{id}/nullifiers/chunks/*` mirrors the commitment feed: wallets
-- pull the whole spent set in fixed-size immutable chunks and filter
-- locally, instead of POSTing the nullifiers they care about (which handed
-- the server the wallet's note set).
--
-- `notes` gets its chunk key for free from the contract via `leaf_index`.
-- `NullifierConsumed` carries no index, so fmd-indexer assigns `seq` at
-- insert: `max(seq) + 1` over new rows ordered by (block_number, log_index).
-- Reorg cleanup (`DELETE … WHERE block_number >= $cursor`) only trims the
-- tail, so the sequence stays dense and already-complete chunks keep their
-- contents.

ALTER TABLE spent_nullifiers ADD COLUMN seq BIGINT;

UPDATE spent_nullifiers s
SET seq = o.n
FROM (
    SELECT chain_id,
           block_number,
           log_index,
           row_number() OVER (PARTITION BY chain_id
                              ORDER BY block_number, log_index) - 1 AS n
    FROM spent_nullifiers
) o
WHERE s.chain_id = o.chain_id
  AND s.block_number = o.block_number
  AND s.log_index = o.log_index;

ALTER TABLE spent_nullifiers ALTER COLUMN seq SET NOT NULL;

CREATE UNIQUE INDEX spent_nullifiers_chain_seq_idx
    ON spent_nullifiers (chain_id, seq);
