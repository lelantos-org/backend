# chain-types

Solidity event ABI bindings and the decoder that turns an EVM log into a typed
`DecodedEvent`. Pure data: no database, no IO, no FMD crypto.

`ingester` is the only writer of `raw_events` and delegates all decoding here,
so every consumer downstream reads rows produced by exactly one implementation
of the ABI.

## Events

`abi.rs` declares the pool's log types with alloy's `sol!`; `decode.rs` maps
each to a `shared::entities::EventKind` discriminant, which is what
`raw_events.event_kind` stores.

| `EventKind` | Discriminant | Solidity event | Consumed by |
|-------------|--------------|----------------|-------------|
| `NoteCreated` | 1 | `NotePayload` | fmd-indexer |
| `AssetRegistered` | 2 | `AssetRegistered` | explorer-indexer |
| `RootAdvanced` | 3 | `RootAdvanced` | explorer-indexer |
| `AssetMoved` | 4 | `AssetMoved` | explorer-indexer |
| `NullifierConsumed` | 5 | `NullifierConsumed` | fmd-indexer |
| `DepositEscrowed` | 6 | `DepositEscrowed` | fmd-indexer, explorer-indexer |
| `DepositFlushed` | 7 | `DepositFlushed` | explorer-indexer |
| `DepositCanceled` | 8 | `DepositCanceled` | explorer-indexer |

The discriminants are persisted, so they are append-only: renumbering one
silently relabels every historical row.

`NotePayload` is **not** emitted on the escrow path. A deposit's note reaches
the tree only when the relayer flushes it, so `DepositEscrowed` carries the
commitment and the value-commitment coordinates itself — which is what lets
`fmd-indexer` hold the note pending until the flush that commits it lands.

## Decoding

```rust
let decoded = chain_types::decode(kind, &log_data)?;
```

`decode` takes the stored `EventKind` and the raw `LogData` and fails rather
than guessing: an unknown discriminant is `DecodeError::UnknownKind`, a payload
that does not match the ABI is `DecodeError::Alloy`.

## Tests

- `tests/decode_roundtrip.rs` encodes and re-decodes every event.
- `tests/sig_check.rs` prints each event's topic0. It asserts nothing — run it
  with `cargo test -p chain-types -- --nocapture print_sigs` to read the
  selectors off, e.g. when cross-checking against a deployed contract.

## Layering

May import `shared` and alloy. Must NOT import `database`, `fmd-crypto`, or any
binary or service crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
