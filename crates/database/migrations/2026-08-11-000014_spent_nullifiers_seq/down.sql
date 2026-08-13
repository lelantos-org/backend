DROP INDEX spent_nullifiers_chain_seq_idx;

ALTER TABLE spent_nullifiers DROP COLUMN seq;
