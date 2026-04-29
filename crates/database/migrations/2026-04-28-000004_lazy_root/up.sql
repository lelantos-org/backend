-- MASP v2 lazy-root realignment.
--
-- - Drop tables whose source events no longer exist on chain:
--     nullifiers     (NullifierUsed event removed)
--     transactions   (Transact event removed; per-tx amounts/recipient now
--                     private, no chain witness)
--     asset_flow_daily (no public flow source)
--
-- - Adjust `notes`:
--     drop clue_bits_u16  (decoded from ciphertext[0..2] at read time)
--     add  leaf_index     (assigned by fmd-indexer once the matching
--                          RootAdvanced lands; cm0 = startIndex,
--                          cm1 = startIndex + 1)
--
-- - Add `tree_advances` (root history written by explorer-indexer; read by
--   fmd-indexer for cm → leaf_index correlation and by relayer for path
--   verification).

DROP TABLE IF EXISTS asset_flow_daily;
DROP TABLE IF EXISTS transactions;
DROP TABLE IF EXISTS nullifiers;

ALTER TABLE notes DROP COLUMN clue_bits_u16;
ALTER TABLE notes ADD COLUMN leaf_index BIGINT NOT NULL;
CREATE UNIQUE INDEX notes_chain_leaf_idx ON notes (chain_id, leaf_index);

CREATE TABLE tree_advances (
    chain_id     BIGINT  NOT NULL,
    block_number BIGINT  NOT NULL,
    log_index    INTEGER NOT NULL,
    start_index  BIGINT  NOT NULL,
    inserted     INTEGER NOT NULL,
    old_root     BYTEA   NOT NULL,
    new_root     BYTEA   NOT NULL,
    tx_hash      BYTEA   NOT NULL,
    block_ts     BIGINT  NOT NULL,
    PRIMARY KEY (chain_id, block_number, log_index)
);
CREATE INDEX tree_advances_chain_start_idx ON tree_advances (chain_id, start_index);
CREATE INDEX tree_advances_chain_root_idx  ON tree_advances (chain_id, new_root);
