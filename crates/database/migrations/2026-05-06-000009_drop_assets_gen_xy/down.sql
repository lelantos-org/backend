-- Restore the gen_x / gen_y columns. Backfill with 0 so existing rows
-- continue to satisfy NOT NULL; the on-chain registry no longer carries
-- the generator, so older indexer code rolling back must be aware these
-- values are not load-bearing.
ALTER TABLE assets ADD COLUMN gen_x NUMERIC(78, 0) NOT NULL DEFAULT 0;
ALTER TABLE assets ADD COLUMN gen_y NUMERIC(78, 0) NOT NULL DEFAULT 0;
