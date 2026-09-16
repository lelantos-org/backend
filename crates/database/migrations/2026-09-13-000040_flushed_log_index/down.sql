DROP INDEX IF EXISTS tree_advances_chain_tx_idx;
ALTER TABLE deposit_escrowed_events DROP COLUMN flushed_log_index;
