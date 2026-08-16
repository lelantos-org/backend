-- Support for classifying transactions as deposit / pending / transfer /
-- withdraw.
--
-- Two of the four kinds are already derivable. `AssetMoved` is emitted from
-- exactly two sites in MASP.sol and they are mutually exclusive — `withdraw()`
-- emits (0, outAmt) and `_finalizeDeposit()` emits (inAmt, 0), and every spend
-- entry point forces `publicIn == 0` — so the sign of an `asset_flows` row is
-- the label. Escrowed-but-unflushed deposits are likewise already visible as
-- `flushed_at_block IS NULL AND canceled_at_block IS NULL`.
--
-- What is missing is telling a `transfer` from a `flushBatch`. Both advance the
-- tree and move no tokens, so they are indistinguishable in `tree_advances`.
-- The discriminator is `DepositFlushed`, which the indexer already consumes but
-- whose transaction it discarded, keeping only the block number. Matching on
-- block alone breaks the moment one block holds both a flush and a transfer.
ALTER TABLE deposit_escrowed_events ADD COLUMN flushed_at_ts   BIGINT;
ALTER TABLE deposit_escrowed_events ADD COLUMN flushed_tx_hash BYTEA;

-- Anti-join support: "tree advances whose tx is not a flush".
CREATE INDEX deposit_escrowed_flushed_tx_idx
    ON deposit_escrowed_events (chain_id, flushed_tx_hash)
    WHERE flushed_tx_hash IS NOT NULL;

-- Deposits are counted at flush time, so the deposit series buckets on this.
CREATE INDEX deposit_escrowed_flushed_ts_idx
    ON deposit_escrowed_events (chain_id, flushed_at_ts)
    WHERE flushed_at_ts IS NOT NULL;

-- Pending deposits are read by escrow time and by outstanding-ness.
CREATE INDEX deposit_escrowed_pending_idx
    ON deposit_escrowed_events (chain_id, block_ts)
    WHERE flushed_at_block IS NULL AND canceled_at_block IS NULL;

-- The classified feed is newest-first and `tree_advances` had no block_ts
-- index at all, which is also why the UI resorted to paging the oldest rows
-- forward to find recent ones. `asset_flows` already has the equivalent index
-- from 000006, and a btree scans backwards, so ASC serves ORDER BY … DESC.
CREATE INDEX tree_advances_chain_ts_idx ON tree_advances (chain_id, block_ts);
