-- Per-leg fee rates for each registered asset.
--
-- Fees are per asset and per leg on chain, with no pool-wide fallback: the
-- contract holds `depositBps` and `withdrawBps` in each `AssetEntry`, and a
-- stored 0 is a real zero rather than "unset". There is therefore no default a
-- consumer may assume, and no single value to cache per chain.
--
-- NULL means "no `AssetFeeSet` observed yet", the same contract `decimals` uses
-- for an unfetched read. It differs from `decimals` in how it gets filled: a
-- fee is mutable, so it cannot be backfilled once over RPC — the indexer
-- follows `AssetFeeSet`, which the contract emits at registration and on every
-- change. Consumers must treat NULL as unknown and decline to quote a fee,
-- never as 0.
ALTER TABLE assets ADD COLUMN deposit_bps SMALLINT;
ALTER TABLE assets ADD COLUMN withdraw_bps SMALLINT;
