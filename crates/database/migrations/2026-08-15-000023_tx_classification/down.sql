DROP INDEX tree_advances_chain_ts_idx;
DROP INDEX deposit_escrowed_pending_idx;
DROP INDEX deposit_escrowed_flushed_ts_idx;
DROP INDEX deposit_escrowed_flushed_tx_idx;
ALTER TABLE deposit_escrowed_events DROP COLUMN flushed_tx_hash;
ALTER TABLE deposit_escrowed_events DROP COLUMN flushed_at_ts;
