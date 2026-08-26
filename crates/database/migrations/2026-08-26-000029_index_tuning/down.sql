DROP INDEX IF EXISTS matches_note_idx;

CREATE INDEX IF NOT EXISTS matches_sub_idx   ON matches (subscription_id, note_id DESC);
CREATE INDEX IF NOT EXISTS matches_chain_idx ON matches (chain_id, note_id);
DROP INDEX IF EXISTS matches_sub_chain_note_idx;

DROP INDEX IF EXISTS notes_chain_leaf_idx;
CREATE UNIQUE INDEX notes_chain_leaf_idx ON notes (chain_id, leaf_index);

DROP INDEX IF EXISTS spent_nullifiers_chain_seq_idx;
CREATE UNIQUE INDEX spent_nullifiers_chain_seq_idx ON spent_nullifiers (chain_id, seq);

CREATE INDEX IF NOT EXISTS asset_locked_chain_idx ON asset_locked (chain_id);
