-- Hashing is one-way, so this rollback cannot restore the replaced tokens.
--
-- Retaining the hashes in a column the prior code reads as raw tokens would
-- invalidate every client's token and promote the stored hash to the
-- credential in its place. The column is cleared instead: a NULL token never
-- matches a lookup, so those rows keep filtering and stay unreachable through
-- the token endpoints until their owners re-register.

ALTER INDEX subscriptions_token_hash_idx RENAME TO subscriptions_token_idx;

ALTER TABLE subscriptions RENAME COLUMN token_hash TO token;

UPDATE subscriptions SET token = NULL;
