-- `IntentEscrowed` now carries `feeBpsAtSubmit` so the relayer can rebuild
-- the on-chain digest at flush/cancel time. Indexers persist this field
-- per intent.
ALTER TABLE intent_escrowed_events
    ADD COLUMN fee_bps_at_submit INT NOT NULL DEFAULT 0;
