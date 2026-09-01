use alloy::primitives::{Address, address};
use alloy::sol;

sol! {
    /// Encrypted-note payload for the spend path, emitted once per output leaf
    /// by `MASP._emitNotes`. Carries the FMD clue, the ECDH ephemeral pubkey,
    /// the ciphertext and the leaf's Pedersen value commitment. `cm` is
    /// indexed, making this log the note-creation signal for indexers that
    /// track commitments only.
    #[derive(Debug)]
    event NotePayload(
        bytes32 indexed cm,
        uint256 clueRx,
        uint256 clueRy,
        uint256 ephPubX,
        uint256 ephPubY,
        bytes ciphertext,
        uint256 cvDepX,
        uint256 cvDepY
    );

    /// Emitted in the MASP constructor for each registered asset. `token` is
    /// the underlying ERC20; `scale` lifts a circuit `uint64` value into the
    /// smallest token unit. The Baby-Jubjub asset generator is derived
    /// in-circuit via `HashToAssetGen(assetId)`.
    #[derive(Debug)]
    event AssetRegistered(
        uint64 indexed assetId,
        address indexed token,
        uint256 scale
    );

    /// Emitted with the rates an asset is registered at, and again on every
    /// change. Fees are per asset and per leg with no pool-wide fallback, so a
    /// consumer must follow this event rather than reading a rate once:
    /// `AssetRegistered` carries neither value, and unlike `scale` a fee is
    /// mutable. Registration emits both events in the same transaction.
    #[derive(Debug)]
    event AssetFeeSet(
        uint64 indexed assetId,
        uint16 depositBps,
        uint16 withdrawBps
    );

    /// Emitted by `CommitmentTree._advanceRoot` when a tree-update SNARK lands
    /// on chain. `startIndex` is the leaf index of the first newly inserted
    /// commitment; `inserted` is the leaf count — spends insert
    /// `PubInputs.TRANSACT_OUT` leaves and a flush inserts one per deposit, so
    /// an odd count is expected. `oldRoot → newRoot` chains root history.
    ///
    /// Indexers associate the accompanying note events with leaf indices by
    /// (chain_id, block_number, log_index) ordering within a `tx_hash`: the
    /// i-th note of the transaction is leaf `startIndex + i`.
    #[derive(Debug)]
    event RootAdvanced(
        uint64 indexed startIndex,
        uint64 inserted,
        bytes32 oldRoot,
        bytes32 newRoot
    );

    /// Emitted per spend when the gross deposit/withdraw amount is non-zero;
    /// internal transfers emit nothing.
    ///
    /// `inAmount` / `outAmount` are ERC-20 base units — what actually moved.
    /// `publicIn` / `publicOut` are the same movement in circuit units, as the
    /// SNARK published it. Today the two differ only by the asset's `scale`;
    /// once a pool-managed yield index is live the conversion also multiplies
    /// by that index, so `outAmount / scale` no longer recovers the circuit
    /// value and the indexer must read it from the log rather than re-derive
    /// contract arithmetic against a figure that moves every block.
    #[derive(Debug)]
    event AssetMoved(
        uint64 indexed assetId,
        address indexed token,
        uint256 inAmount,
        uint256 outAmount,
        uint64 publicIn,
        uint64 publicOut
    );

    /// Emitted on every nullifier burn. Lets wallets reconcile cached notes
    /// against the on-chain spent set without a per-nullifier `eth_call` to
    /// `spent(bytes32)`.
    #[derive(Debug)]
    event NullifierConsumed(bytes32 indexed nf);

    /// Emitted on `deposit` / `depositAuthorized`. A deposit occupies exactly
    /// one leaf, so this carries a single cm plus that leaf's FMD clue, ECDH
    /// ephemeral pubkey, ciphertext, Pedersen value commitment `cvDep` and its
    /// blinder `rcv`. `NotePayload` is not emitted on the escrow path; this
    /// event is the shielded-note-created signal for shields.
    ///
    /// The event's block number is the deposit's `submittedAt`, which the
    /// relayer replays into `MASP.DepositMeta` at flush time and into the
    /// digest preimage at cancel time.
    #[derive(Debug)]
    event DepositEscrowed(
        uint256 indexed id,
        address indexed payer,
        address indexed recipient,
        uint64 publicAssetId,
        uint64 publicIn,
        uint16 feeBpsAtSubmit,
        bytes32 cm,
        uint256 cvDepX,
        uint256 cvDepY,
        uint256 rcv,
        uint256 clueRx,
        uint256 clueRy,
        uint256 ephPubX,
        uint256 ephPubY,
        bytes ciphertext,
        // The relayer's fee note. Non-indexed: all three topic slots are
        // taken, and a relayer locates its note by trial decryption rather
        // than by filtering.
        uint64 feeIn,
        bytes32 feeCm,
        uint256 feeCvDepX,
        uint256 feeCvDepY,
        uint256 feeRcv,
        uint256 feeClueRx,
        uint256 feeClueRy,
        uint256 feeEphPubX,
        uint256 feeEphPubY,
        bytes feeCiphertext
    );

    /// Emitted per deposit inside `flushBatch`. The full per-note data is
    /// carried by `DepositEscrowed`.
    #[derive(Debug)]
    event DepositFlushed(uint256 indexed id, bytes32 cm);

    /// Emitted by `cancelDeposit`. Carries the refund target and the total
    /// refunded (in + fee, scaled token units).
    #[derive(Debug)]
    event DepositCanceled(uint256 indexed id, address indexed payer, uint256 refunded);

    /// Emitted when an asset id is bound to a yield venue, which happens once
    /// and never again: `MASP.addYieldAsset` goes through the add-only registry,
    /// so a re-registration reverts. This log is therefore the definitive
    /// signal that an asset is yield-bearing, and there is no counterpart
    /// unbinding it.
    #[derive(Debug)]
    event YieldAssetAdded(
        uint64 indexed assetId,
        address indexed venue,
        uint16 bufferBps,
        uint16 perfBps
    );

    /// Owner change to an asset's buffer share or performance-fee rate. Neither
    /// touches the venue binding.
    #[derive(Debug)]
    event YieldParamsSet(
        uint64 indexed assetId,
        uint16 bufferBps,
        uint16 perfBps
    );

    /// The treasury's cut of venue growth, minted as normalized units rather
    /// than moved as tokens — nobody's balance is rewritten, the treasury simply
    /// starts owning a slice of the pot. `newLastIdx` is the fee high-water mark
    /// after the mint.
    ///
    /// Together with `NormalizedFeeSwept` this is the only record of what the
    /// protocol earned: an accrual moves no tokens, so it leaves no `AssetMoved`
    /// and cannot be reconstructed from flows.
    #[derive(Debug)]
    event PerfFeeAccrued(
        uint64 indexed assetId,
        uint256 unitsMinted,
        uint256 newLastIdx
    );

    /// Accrued fee units converted to underlying and sent to the treasury.
    /// `units` is what was burned, `amount` what was paid; they differ by the
    /// index at settlement.
    #[derive(Debug)]
    event NormalizedFeeSwept(
        uint64 indexed assetId,
        uint256 units,
        uint256 amount
    );

    /// The idle/venue split was restored. Moves no value in or out of the pool,
    /// so it changes no balance — only where the asset's backing sits.
    #[derive(Debug)]
    event Rebalanced(uint64 indexed assetId, uint256 idleAfter);

    /// The asset stopped, or resumed, supplying its venue. Emitted by
    /// `emergencyUnwind` (always `true`) and by `setHalted`.
    #[derive(Debug)]
    event HaltedSet(uint64 indexed assetId, bool halted);

    /// A venue position was pulled back to idle. `recovered` may be less than
    /// the position when the vault is short of liquidity, and the call is
    /// repeatable, so several of these can describe one unwind.
    #[derive(Debug)]
    event EmergencyUnwound(uint64 indexed assetId, uint256 recovered);
}

/// Multicall3's canonical address.
///
/// Deterministic-deployed via Nick's method, so the address is a property of the
/// bytecode rather than of the deployer and is identical on every chain that has
/// it. It is a *default*, not a guarantee: a bare `anvil` has no Multicall3, and
/// a chain that deployed its own has it elsewhere — callers take the address
/// from config and use this when none is set.
pub const MULTICALL3: Address = address!("0xcA11bde05977b3631167028862bE2a173976CA11");

sol! {
    /// The aggregator every consumer of this crate batches reads through.
    ///
    /// Here rather than in one adapter because it is not one service's business:
    /// `explorer-indexer` batches its yield round through it today, and the
    /// relayer's per-deposit `digests` fan-out and the metaquoter's fee-tier race
    /// are the same shape. A second copy would mean a second 20-byte address to
    /// get right, and a wrong one does not error — it reads as "no Multicall3"
    /// and silently runs slow.
    #[sol(rpc)]
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
        struct Result {
            bool success;
            bytes returnData;
        }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
        function getBlockNumber() external view returns (uint256 blockNumber);
    }
}
