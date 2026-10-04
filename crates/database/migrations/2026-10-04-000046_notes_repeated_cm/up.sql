-- Two leaves may hold the same commitment: a deposit repeating an earlier
-- `(asset, value, inner)` inserts a second leaf with the same `cm`, and the pool
-- accepts it. `UNIQUE (chain_id, cm)` made the indexer drop that leaf's row,
-- which leaves a gap in `leaf_index` that the commitment feed refuses to serve.
--
-- A note is identified by its position. `notes_chain_leaf_idx`, UNIQUE on
-- `(chain_id, leaf_index)`, is what the indexer's insert conflicts on. No query
-- looks a note up by `cm`, so the index behind the constraint is not replaced.
ALTER TABLE notes DROP CONSTRAINT notes_chain_id_cm_key;
