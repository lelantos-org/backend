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
    /// internal transfers emit nothing. `inAmount` / `outAmount` are scaled
    /// token base units.
    #[derive(Debug)]
    event AssetMoved(
        uint64 indexed assetId,
        address indexed token,
        uint256 inAmount,
        uint256 outAmount
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
}
