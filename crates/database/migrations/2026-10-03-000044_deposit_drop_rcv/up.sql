-- `tree_update_batch` fixes a deposit leaf's blinder at 1: the binding is
-- `cv_dep == public_in · V^asset + H`, with no blinder witness. `DepositRequest`
-- loses `rcv` / `feeRcv` and `DepositEscrowed` no longer publishes them, so the
-- two columns have no source and no reader: the flush witness carries no
-- blinder, and neither was ever digest preimage.
--
-- OPERATIONAL NOTE: this ships with a fresh pool deployment and new circuit
-- keys, like 000028, 000041 and 000043: the event layout and its topic0 and the
-- `deposit` / `depositAuthorized` selectors all change, and no compatibility
-- path is kept.
--
-- Existing rows are kept, as in 000043. They are the old pool's escrows, whose
-- leaves were committed under the blinder dropped here. Nothing can act on them:
-- the old pool is flushed by the old circuit, which this binary no longer
-- proves. Clear the table with the rest of the old deployment's derived state
-- when re-ingesting from the new deploy block.

ALTER TABLE deposit_escrowed_events
    DROP COLUMN rcv,
    DROP COLUMN fee_rcv;
