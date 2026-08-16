-- `block_number` is the chain's own block height, which is what eth_getLogs,
-- reorg detection and the ingest cursor all work in. On most chains that is
-- also what Solidity's `block.number` returns.
--
-- Arbitrum is the exception: inside the EVM, `block.number` yields an
-- approximation of the *L1* height, not the L2 height the receipt reports.
-- MASP folds `uint32(block.number)` into the deposit digest, so replaying an
-- Arbitrum deposit with its L2 block number reconstructs a different digest
-- and `flushBatch` reverts `DigestMismatch(id)` forever.
--
-- `evm_block_number` records what the contract actually saw. It equals
-- `block_number` everywhere except Arbitrum-style chains.
--
-- Nullable on purpose: rows written before this migration have no reliable
-- value, and readers COALESCE to `block_number`. That is correct for every
-- chain except Arbitrum, whose affected rows must be repaired explicitly —
-- guessing here would bake in the very error this column exists to fix.
ALTER TABLE raw_events ADD COLUMN evm_block_number BIGINT;

COMMENT ON COLUMN raw_events.evm_block_number IS
    'Block number as observed by the EVM (Solidity block.number). Differs from block_number on Arbitrum, where block.number is the L1 height. NULL for rows ingested before this column existed.';
