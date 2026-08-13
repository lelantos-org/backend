-- Drop the UNIQUE constraint on `subscriptions.detection_key`.
--
-- Nothing depends on it: `create` is a plain INSERT with no ON CONFLICT
-- clause, and two subscriptions sharing a key would only produce duplicate
-- `matches` rows under different `subscription_id`s, which is harmless.
--
-- It costs privacy on two counts. A unique index keeps a second copy of the
-- key material in index pages and in the WAL, widening what a backup or a
-- page-level disclosure exposes. And it answers "is this detection key
-- already registered?" in O(log n) for anyone who can reach an insert path.

ALTER TABLE subscriptions DROP CONSTRAINT IF EXISTS subscriptions_detection_key_key;
