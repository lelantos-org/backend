-- A note commits to its asset and value by hash,
-- `cm = Poseidon(TAG_CM, asset * 2^64 + value, inner)`, and `cm` is the tree
-- leaf. There is no value commitment: `NotePayload` and `DepositEscrowed` carry
-- no `cvDep`, so the `cv_dep` columns have no source.
--
-- `DepositEscrowed` publishes `inner` (and `feeInner`), not a commitment. The
-- leaf a flush inserts is computed from `(public_asset_id, public_in, inner)`,
-- and likewise for the fee note, so the two event columns are renamed to what
-- they hold. `notes.cm` is the leaf for every note, deposits included.
--
-- OPERATIONAL NOTE: this ships with a fresh pool deployment and new circuit
-- keys, like 000043 and 000044. Existing rows are the old pool's and are kept;
-- clear them with the rest of the old deployment's derived state when
-- re-ingesting from the new deploy block.

-- Dropping a column drops every index that carries it, INCLUDE columns too, so
-- the covering index of 000029 is recreated over the payload that remains. It
-- is also the UNIQUE constraint on `(chain_id, leaf_index)`.
DROP INDEX IF EXISTS notes_chain_leaf_idx;

ALTER TABLE notes
    DROP COLUMN cv_dep_x,
    DROP COLUMN cv_dep_y;

CREATE UNIQUE INDEX notes_chain_leaf_idx
    ON notes (chain_id, leaf_index) INCLUDE (cm);

ALTER TABLE deposit_escrowed_events
    DROP COLUMN cv_dep_x,
    DROP COLUMN cv_dep_y,
    DROP COLUMN fee_cv_dep_x,
    DROP COLUMN fee_cv_dep_y;

-- `inner` is a reserved word, so raw SQL naming the column quotes it.
ALTER TABLE deposit_escrowed_events RENAME COLUMN cm TO "inner";
ALTER TABLE deposit_escrowed_events RENAME COLUMN fee_cm TO fee_inner;
