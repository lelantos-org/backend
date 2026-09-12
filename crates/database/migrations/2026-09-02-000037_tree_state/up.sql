-- Persisted commitment-tree state, written by fmd-indexer, read by fmd-webserver.
--
-- Before this, every fmd-webserver process rebuilt the whole quaternary tree in
-- memory from `notes` to answer `/v1/tree-state`, which only ever returns the
-- leaf count, the root and the frontier. That cost ~180 MB per chain per replica
-- at the tree's 4^11 capacity, paid the O(N) Poseidon cold build on the request
-- path, and broke permanently on a `leaf_index` hole -- which `fmd-indexer` can
-- legitimately produce for a leaf whose ciphertext is too short to decode, and
-- which no restart repairs.
--
-- The frontier is an append-only tree's complete resume state, which is why the
-- contract stores `filledSubtrees` and nothing else. So one row per chain, about
-- a kilobyte, replaces the mirrors: fmd-indexer already sees the leaf the chain
-- inserted (the undecodable note still carries `cm` and `cv_dep`), so it can
-- advance a frontier across a hole that a reader of `notes` cannot.
--
-- Root *history* is deliberately absent: `tree_advances.new_root` already holds
-- it, straight from the chain, and doubles as the oracle this table is checked
-- against.
CREATE TABLE tree_state (
    -- Latest state only, so the chain is the whole key.
    chain_id   BIGINT      PRIMARY KEY,
    leaf_count BIGINT      NOT NULL,
    -- 32-byte field element.
    root       BYTEA       NOT NULL,
    -- DEPTH(11) * 3 * 32 = 1056 bytes, concatenated big-endian field elements in
    -- the on-chain `filledSubtrees` layout. One column rather than BYTEA[]: it is
    -- fixed-width, read as a unit, and length-checked on the way out.
    frontier   BYTEA       NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
