//! The decoded form of every event this crate reads.

use alloy::primitives::{Address, B256, U256};

/// One of the two notes a `DepositEscrowed` publishes.
#[derive(Debug, Clone)]
pub struct DepositNote {
    pub asset_id: u64,
    pub value: u64,
    /// `inner`; the leaf is `crypto::note::commitment_from_inner`.
    pub inner: B256,
    pub clue_rx: U256,
    pub clue_ry: U256,
    pub eph_pub_x: U256,
    pub eph_pub_y: U256,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum DecodedEvent {
    NoteCreated {
        cm: B256,
        clue_rx: U256,
        clue_ry: U256,
        eph_pub_x: U256,
        eph_pub_y: U256,
        ciphertext: Vec<u8>,
    },
    AssetRegistered {
        asset_id: u64,
        token: Address,
        scale: U256,
    },
    /// Per-leg fee rates for one asset. Emitted at registration and on every
    /// change; there is no pool-wide rate to fall back to, so an asset with no
    /// observed `AssetFeeSet` has unknown rates rather than default ones.
    AssetFeeSet {
        asset_id: u64,
        deposit_bps: u16,
        withdraw_bps: u16,
    },
    RootAdvanced {
        start_index: u64,
        inserted: u64,
        old_root: B256,
        new_root: B256,
    },
    AssetMoved {
        asset_id: u64,
        token: Address,
        /// ERC-20 base units moved.
        in_amount: U256,
        out_amount: U256,
        /// The same movement in circuit units, as published by the SNARK.
        public_in: u64,
        public_out: u64,
    },
    NullifierConsumed {
        nf: B256,
    },
    DepositEscrowed {
        id: U256,
        payer: Address,
        recipient: Address,
        fee_bps_at_submit: u16,
        /// The depositor's note, the deposit's first leaf.
        note: DepositNote,
        /// The relayer's fee note, the second leaf. Its asset is independent of
        /// `note`'s and 0 exactly when its value is 0.
        fee: DepositNote,
        /// The refund cap, digest preimage; see [`crate::abi::DepositEscrowed`].
        pulled: U256,
    },
    DepositFlushed {
        id: U256,
        inner: B256,
    },
    DepositCanceled {
        id: U256,
        payer: Address,
        /// Refunded in the deposit token.
        refunded: U256,
        /// The fee note's asset (0 for a zero-fee deposit).
        fee_asset_id: u64,
        /// Refunded in `fee_asset_id`'s token; nonzero only when the fee was
        /// paid in a different asset than the deposit.
        fee_refunded: U256,
    },
    /// An asset id bound to a yield venue. Emitted once per asset and never
    /// reversed, so this is what makes an asset yield-bearing for a consumer.
    YieldAssetAdded {
        asset_id: u64,
        venue: Address,
        buffer_bps: u16,
        perf_bps: u16,
    },
    YieldParamsSet {
        asset_id: u64,
        buffer_bps: u16,
        perf_bps: u16,
    },
    /// The treasury's cut of growth, minted as units. Moves no tokens, so it
    /// has no `AssetMoved` counterpart and is not derivable from flows.
    PerfFeeAccrued {
        asset_id: u64,
        units_minted: U256,
        new_last_idx: U256,
    },
    NormalizedFeeSwept {
        asset_id: u64,
        units: U256,
        amount: U256,
    },
    Rebalanced {
        asset_id: u64,
        idle_after: U256,
    },
    HaltedSet {
        asset_id: u64,
        halted: bool,
    },
    EmergencyUnwound {
        asset_id: u64,
        recovered: U256,
    },
    /// A governor proposal. `calldatas[i]` is sent to `targets[i]` with
    /// `values[i]` wei; `signatures` is kept for OZ's ABI shape and is empty
    /// strings for proposals made through `propose`.
    ProposalCreated {
        proposal_id: U256,
        proposer: Address,
        targets: Vec<Address>,
        values: Vec<U256>,
        signatures: Vec<String>,
        calldatas: Vec<Vec<u8>>,
        vote_start: U256,
        vote_end: U256,
        description: String,
    },
    /// After this instant only Against votes are accepted.
    ProposalQuorumVoteDeadline {
        proposal_id: U256,
        quorum_vote_deadline: U256,
    },
    /// Both `VoteCast` and `VoteCastWithParams`: the two differ only in
    /// `params`, which is `None` for the former.
    VoteCast {
        voter: Address,
        proposal_id: U256,
        /// 0 Against, 1 For, 2 Abstain.
        support: u8,
        weight: U256,
        reason: String,
        params: Option<Vec<u8>>,
    },
    ProposalQueued {
        proposal_id: U256,
        eta_seconds: U256,
    },
    ProposalExecuted {
        proposal_id: U256,
    },
    ProposalCanceled {
        proposal_id: U256,
    },
}
