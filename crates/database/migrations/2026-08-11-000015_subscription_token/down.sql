DROP INDEX IF EXISTS subscriptions_token_idx;
ALTER TABLE subscriptions DROP COLUMN token;
