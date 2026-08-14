use alloy::sol;

sol! {
    /// Encrypted-note payload for the spend path, emitted once per output
    /// leaf by `MASP._emitNotes`. Carries the FMD clue, the ECDH ephemeral
    /// pubkey, the ciphertext and the leaf's Pedersen value commitment.
    /// `cm` is indexed, so this log doubles as the note-creation signal for
    /// indexers that track commitments only — there is no separate
    /// `NotesCreated` event.
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

    /// Emitted in MASP constructor for each registered asset. `token` is the
    /// underlying ERC20; `scale` lifts circuit `uint64` value into smallest
    /// token unit. The Baby-Jubjub asset generator is derived in-circuit via
    /// `HashToAssetGen(assetId)` and is no longer stored on-chain.
    #[derive(Debug)]
    event AssetRegistered(
        uint64 indexed assetId,
        address indexed token,
        uint256 scale
    );

    /// Emitted by CommitmentTree._advanceRoot whenever a tree-update SNARK
    /// lands on chain. `startIndex` is the leaf index of the first newly
    /// inserted commitment; `inserted` is the leaf count — spends insert
    /// `PubInputs.TRANSACT_OUT` leaves, a flush inserts one per deposit, so
    /// an odd count is normal. `oldRoot → newRoot` lets indexers chain root
    /// history.
    ///
    /// Backend uses (chain_id, block_number, log_index) ordering plus
    /// (tx_hash) to associate the accompanying note events with their leaf
    /// indices: the i-th note of the tx is leaf `startIndex + i`.
    #[derive(Debug)]
    event RootAdvanced(
        uint64 indexed startIndex,
        uint64 inserted,
        bytes32 oldRoot,
        bytes32 newRoot
    );

    /// Emitted once per spend whenever the gross deposit/withdraw amount is
    /// non-zero. Internal transfers emit nothing. `inAmount` / `outAmount`
    /// are token base units (already scaled).
    #[derive(Debug)]
    event AssetMoved(
        uint64 indexed assetId,
        address indexed token,
        uint256 inAmount,
        uint256 outAmount
    );

    /// Emitted on every nullifier burn. Indexed off-chain so wallets can
    /// reconcile cached notes against the on-chain spent set without per-nf
    /// `eth_call` to `spent(bytes32)`.
    #[derive(Debug)]
    event NullifierConsumed(bytes32 indexed nf);

    /// Emitted on `deposit` / `depositAuthorized`. A deposit occupies exactly
    /// one leaf, so this carries a single cm plus that leaf's FMD clue, ECDH
    /// eph pub, ciphertext, Pedersen value commitment `cvDep` and its blinder
    /// `rcv` — everything the relayer needs to assemble a `flushBatch`.
    /// `NotePayload` is NOT emitted on the escrow path; this event is the
    /// canonical "shielded note created" signal for shields.
    ///
    /// The event's block number is the deposit's `submittedAt`, which the
    /// relayer must replay into `MASP.DepositMeta` at flush time and into the
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
        bytes ciphertext
    );

    /// Emitted per-deposit inside `flushBatch`. (id, cm) — the full per-note
    /// data was emitted at deposit time by `DepositEscrowed`.
    #[derive(Debug)]
    event DepositFlushed(uint256 indexed id, bytes32 cm);

    /// Emitted by `cancelDeposit`. Refund target + total refunded
    /// (in + fee, scaled token units).
    #[derive(Debug)]
    event DepositCanceled(uint256 indexed id, address indexed payer, uint256 refunded);
}
