-- Index the escrow lookup fmd-indexer runs on every consume tick that sees a
-- `DepositFlushed`.
--
-- `fetch_escrowed_by_ids` matches `topics[2]` (the indexed deposit id). No
-- index covered that expression, so the query fell back to scanning every
-- `DepositEscrowed` row for the chain — a cost that grows with chain history
-- forever, paid per tick, to look up a handful of ids.
--
-- Partial on the event kind: `DepositEscrowed` is a small slice of
-- `raw_events`, and the consume query always pins the kind.
CREATE INDEX raw_events_escrowed_id_idx
    ON raw_events (chain_id, (topics[2]))
    WHERE event_kind = 6;  -- EventKind::DepositEscrowed
