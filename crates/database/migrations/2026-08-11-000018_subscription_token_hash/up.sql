-- Store SHA-256 of the capability token instead of the token itself.
--
-- The column holds a bearer credential, so a backup, a replica or a
-- page-level disclosure yields working capabilities for every subscription.
-- Equality lookup is the only operation the server performs on the value,
-- and a digest serves it.
--
-- Unsalted and unstretched: the input is a 32-byte uniform secret, not a
-- password, so there is no dictionary to precompute.
--
-- Existing rows are rehashed in place so tokens already held by clients
-- remain valid. `sha256(bytea)` is a Postgres 11+ builtin.

ALTER TABLE subscriptions RENAME COLUMN token TO token_hash;

UPDATE subscriptions SET token_hash = sha256(token_hash) WHERE token_hash IS NOT NULL;

ALTER INDEX subscriptions_token_idx RENAME TO subscriptions_token_hash_idx;
