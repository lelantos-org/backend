-- Index tuning for the reorg, tree-mirror and match-listing paths.
--
-- Every change below was measured on a synthetic dataset at 3M `raw_events`,
-- 1.5M `notes` and 3M `matches`; the figures in each comment are from that run.
--
-- On a deployment where these tables are already large, build the new indexes
-- out of band first:
--
--     CREATE INDEX CONCURRENTLY ...
--
-- and this migration then no-ops on them. It runs at process startup behind the
-- migration advisory lock, and a standby gives up on that lock after 120s, so a
-- long in-migration build would fail the deploy rather than block on it.

-- 1. `matches.note_id` has no index, so the `ON DELETE CASCADE` from `notes`
--    seq-scans `matches` once per deleted row. Reorg retraction deletes notes by
--    block range, so this is the reorg path: 20 notes took 1476ms, and 15k notes
--    did not finish inside two minutes. With the index, the same 15k retraction
--    is 55ms.
--
--    Neither existing index helps: both `matches_sub_idx` and `matches_chain_idx`
--    lead with another column, and the primary key is (subscription_id, note_id).
CREATE INDEX IF NOT EXISTS matches_note_idx ON matches (note_id);

-- 2. `list_for_subscription` filters (subscription_id, chain_id, note_id) but
--    `matches_sub_idx` carries no `chain_id`, so every match for the subscription
--    is read and then discarded. Adding `chain_id` makes it index-only: 876
--    buffers to 3.
--
--    This supersedes both older indexes. `matches_sub_idx` duplicated the primary
--    key, which Postgres can scan backwards for the same DESC order, and
--    `matches_chain_idx` served no query in the codebase.
CREATE INDEX IF NOT EXISTS matches_sub_chain_note_idx
    ON matches (subscription_id, chain_id, note_id);
DROP INDEX IF EXISTS matches_sub_idx;
DROP INDEX IF EXISTS matches_chain_idx;

-- 3. The tree mirror replays `(leaf_index, cm, cv_dep)` for a whole chain at
--    relayer startup, and `/v1/commitments` serves bounded slices of the same
--    query. `notes_chain_leaf_idx` covers the predicate but not the payload, so
--    the planner preferred a sequential scan plus a sort that spilled 47MB to
--    disk. Carrying the three payload columns makes it an ordered index-only
--    scan: 327ms to 59ms, 60k buffers to 9k, no sort.
--
--    Recreated under the same name, still UNIQUE on (chain_id, leaf_index):
--    INCLUDE columns are not part of the uniqueness constraint.
DROP INDEX IF EXISTS notes_chain_leaf_idx;
CREATE UNIQUE INDEX notes_chain_leaf_idx
    ON notes (chain_id, leaf_index) INCLUDE (cm, cv_dep_x, cv_dep_y);

-- 4. Same shape for the wallet's spent-nullifier sync, which reads `nf` over a
--    `seq` range. Without the payload the planner used a bitmap scan, which
--    loses index order and forced a sort: 187 buffers and a quicksort become 51
--    buffers index-only.
DROP INDEX IF EXISTS spent_nullifiers_chain_seq_idx;
CREATE UNIQUE INDEX spent_nullifiers_chain_seq_idx
    ON spent_nullifiers (chain_id, seq) INCLUDE (nf);

-- 5. `asset_locked_chain_idx` is a left prefix of the unique `asset_locked_pk`
--    on (chain_id, asset_id_u64), so it can serve no query the primary key does
--    not already serve.
DROP INDEX IF EXISTS asset_locked_chain_idx;
