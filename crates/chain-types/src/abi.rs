use alloy::sol;

sol! {
    /// Slim "note exists" signal — only the cm pair. Encrypted-note payload
    /// moves to the companion `NotePayload` event (emitted on the same
    /// transact/withdraw call). Indexers that only care about tree state
    /// can subscribe to this topic; wallets needing the FMD clue +
    /// ciphertext subscribe to `NotePayload`.
    #[derive(Debug)]
    event NotesCreated(bytes32 indexed cm0, bytes32 indexed cm1);

    /// Full encrypted-note payload for transfer/withdraw flows. Mirrors
    /// the per-output FMD clue + ECDH ephemeral pubkey + ciphertext +
    /// Pedersen value commitment that `IntentEscrowed` carries for
    /// shields. Decoder fans this single log into two per-cm
    /// `DecodedEvent::NoteCreated` entries.
    #[derive(Debug)]
    event NotePayload(
        bytes32 indexed cm0,
        bytes32 indexed cm1,
        uint256 clueRx0,
        uint256 clueRy0,
        uint256 ephPubX0,
        uint256 ephPubY0,
        bytes ciphertext0,
        uint256 clueRx1,
        uint256 clueRy1,
        uint256 ephPubX1,
        uint256 ephPubY1,
        bytes ciphertext1,
        uint256 cvDep0X,
        uint256 cvDep0Y,
        uint256 cvDep1X,
        uint256 cvDep1Y
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
    /// inserted commitment; `inserted` is the count (always 2 in v2).
    /// `oldRoot → newRoot` lets indexers chain root history.
    ///
    /// Backend uses (chain_id, block_number, log_index) ordering plus
    /// (tx_hash) to associate the preceding `NoteCreated` events with their
    /// leaf indices: cm0 = startIndex, cm1 = startIndex + 1.
    #[derive(Debug)]
    event RootAdvanced(
        uint64 indexed startIndex,
        uint64 inserted,
        bytes32 oldRoot,
        bytes32 newRoot
    );

    /// Emitted once per `transact` whenever at least one of the gross
    /// deposit/withdraw amounts is non-zero. Internal transfers emit nothing.
    /// `inAmount` / `outAmount` are token base units (already scaled).
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

    /// Emitted on `submitIntent`. Carries the per-intent payload the relayer
    /// scrapes for batching: cm pair + per-output FMD clue + ECDH eph pub +
    /// ciphertext, plus the per-output Pedersen value commitments
    /// (cvDep0, cvDep1) and the relayer's required `rcvTotal = rcv_dep_0 +
    /// rcv_dep_1` private witness. `NotesCreated` is NOT emitted on the
    /// escrow path — this event is the canonical "shielded note created"
    /// signal for shields.
    #[derive(Debug)]
    event IntentEscrowed(
        uint256 indexed id,
        address indexed payer,
        address indexed recipient,
        uint64 publicAssetId,
        uint64 publicIn,
        uint16 feeBpsAtSubmit,
        bytes32 cm0,
        bytes32 cm1,
        uint256 cvDep0X,
        uint256 cvDep0Y,
        uint256 cvDep1X,
        uint256 cvDep1Y,
        uint256 rcvTotal,
        uint256 clueRx0,
        uint256 clueRy0,
        uint256 ephPubX0,
        uint256 ephPubY0,
        bytes ciphertext0,
        uint256 clueRx1,
        uint256 clueRy1,
        uint256 ephPubX1,
        uint256 ephPubY1,
        bytes ciphertext1
    );

    /// Emitted per-intent inside `flushBatch`. (id, cm0, cm1) — full
    /// per-note data was emitted at submit time by `IntentEscrowed`.
    #[derive(Debug)]
    event IntentFlushed(uint256 indexed id, bytes32 cm0, bytes32 cm1);

    /// Emitted by `cancelIntent`. Refund target + total refunded
    /// (in + fee, scaled token units).
    #[derive(Debug)]
    event IntentCanceled(uint256 indexed id, address indexed payer, uint256 refunded);
}
