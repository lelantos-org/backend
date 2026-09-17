-- On-chain governance: the governor's proposals and votes, plus the emitter of
-- every raw event.
--
-- `raw_events.address` is the contract that emitted the log. topic0 alone does
-- not say which contract that was: the pool and the governor are fetched by one
-- `eth_getLogs` filter now, and OpenZeppelin's governor events carry generic
-- signatures (`ProposalExecuted(uint256)`) any contract could emit. The
-- protocol-indexer only accepts governance events from the governor it is
-- configured with.
--
-- Nullable on purpose, like `evm_block_number`: rows ingested before this
-- column existed have no recorded emitter, and every one of them came from the
-- pool, which was then the only address the ingester fetched.
ALTER TABLE raw_events ADD COLUMN address BYTEA;

COMMENT ON COLUMN raw_events.address IS
    'Emitting contract (20 bytes). NULL for rows ingested before this column existed, all of which came from the pool.';

-- One row per proposal, from `ProposalCreated`.
--
-- `proposal_id` is OZ's `hashProposal` output, a full uint256. The action
-- arrays are parallel: action i is `(targets[i], call_values[i],
-- signatures[i], calldatas[i])`. `call_values` rather than `values`, which is
-- an SQL keyword.
--
-- `vote_start`, `vote_end`, `quorum_vote_deadline` and `eta` are in the governor's
-- clock, which for `LelantosToken` is unix seconds, so BIGINT holds them.
--
-- `block_number`/`log_index`/`tx_hash` are the creating log. The lifecycle
-- columns are set by later events and NULL until then; `quorum_vote_deadline` by
-- `ProposalQuorumVoteDeadline`, emitted in the creating transaction.
--
-- Pending/Active/Defeated/Succeeded are deliberately absent: they depend on the
-- clock and on quorum at the snapshot, and a client reads `state()` on chain.
CREATE TABLE gov_proposals (
    chain_id             BIGINT           NOT NULL,
    proposal_id          NUMERIC(78, 0)   NOT NULL,
    proposer             BYTEA            NOT NULL,
    targets              BYTEA[]          NOT NULL,
    call_values          NUMERIC(78, 0)[] NOT NULL,
    signatures           TEXT[]           NOT NULL,
    calldatas            BYTEA[]          NOT NULL,
    description          TEXT             NOT NULL,
    vote_start           BIGINT           NOT NULL,
    vote_end             BIGINT           NOT NULL,
    quorum_vote_deadline BIGINT,
    block_number         BIGINT           NOT NULL,
    log_index            INTEGER          NOT NULL,
    tx_hash              BYTEA            NOT NULL,
    block_ts             BIGINT           NOT NULL,
    queued_at_block      BIGINT,
    eta                  BIGINT,
    executed_at_block    BIGINT,
    canceled_at_block    BIGINT,

    PRIMARY KEY (chain_id, proposal_id)
);

-- The list route's order: newest first.
CREATE INDEX gov_proposals_chain_created_idx
    ON gov_proposals (chain_id, block_number DESC, log_index DESC);

-- One row per vote. `GovernorCountingSimple` refuses a second vote from the
-- same account, so `(chain_id, proposal_id, voter)` is the key and a replayed
-- window converges on `ON CONFLICT DO NOTHING`.
--
-- No foreign key to `gov_proposals`: a governor deployed below the ingester's
-- start block has votes on proposals this table never saw, and those are kept
-- rather than failing the tick.
--
-- `support` 0 Against, 1 For, 2 Abstain. `params` is NULL for `VoteCast` and
-- set for `VoteCastWithParams`.
CREATE TABLE gov_votes (
    chain_id      BIGINT          NOT NULL,
    proposal_id   NUMERIC(78, 0)  NOT NULL,
    voter         BYTEA           NOT NULL,
    support       SMALLINT        NOT NULL,
    weight        NUMERIC(78, 0)  NOT NULL,
    reason        TEXT            NOT NULL,
    params        BYTEA,
    block_number  BIGINT          NOT NULL,
    log_index     INTEGER         NOT NULL,
    tx_hash       BYTEA           NOT NULL,
    block_ts      BIGINT          NOT NULL,

    PRIMARY KEY (chain_id, proposal_id, voter)
);

-- The votes route's order, and the tally's `GROUP BY` scan.
CREATE INDEX gov_votes_proposal_idx
    ON gov_votes (chain_id, proposal_id, block_number DESC, log_index DESC);

-- Reorg retraction deletes by block on both tables.
CREATE INDEX gov_votes_chain_block_idx ON gov_votes (chain_id, block_number);
