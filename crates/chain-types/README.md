# chain-types

Solidity ABI bindings, the decoder that turns an EVM log into a typed
`DecodedEvent`, and the `U256` <-> `NUMERIC` conversions those values are stored
through. Pure data by default: no database, no IO, no FMD crypto.

Behind the `rpc` feature it also holds `RpcEndpoint`, the shared JSON-RPC
transport — see [The `rpc` feature](#the-rpc-feature).

`ingester` is the only writer of `raw_events` and delegates all decoding here,
so every consumer downstream reads rows produced by exactly one implementation
of the ABI.

## Events

`abi/events.rs` declares the pool's log types with alloy's `sol!`; `decode/` maps
each to a `shared::entities::EventKind` discriminant, which is what
`raw_events.event_kind` stores.

| `EventKind` | Discriminant | Solidity event | Consumer |
|-------------|--------------|----------------|----------|
| `NoteCreated` | 1 | `NotePayload` | Fmd |
| `AssetRegistered` | 2 | `AssetRegistered` | Protocol |
| `RootAdvanced` | 3 | `RootAdvanced` | Protocol |
| `AssetMoved` | 4 | `AssetMoved` | Explorer |
| `NullifierConsumed` | 5 | `NullifierConsumed` | Fmd |
| `DepositEscrowed` | 6 | `DepositEscrowed` | Protocol |
| `DepositFlushed` | 7 | `DepositFlushed` | Protocol |
| `DepositCanceled` | 8 | `DepositCanceled` | Protocol |
| `AssetFeeSet` | 9 | `AssetFeeSet` | Protocol |
| `YieldAssetAdded` | 10 | `YieldAssetAdded` | Protocol |
| `YieldParamsSet` | 11 | `YieldParamsSet` | Protocol |
| `PerfFeeAccrued` | 12 | `PerfFeeAccrued` | Explorer |
| `NormalizedFeeSwept` | 13 | `NormalizedFeeSwept` | Explorer |
| `Rebalanced` | 14 | `Rebalanced` | None |
| `HaltedSet` | 15 | `HaltedSet` | Protocol |
| `EmergencyUnwound` | 16 | `EmergencyUnwound` | None |

The consumer column is `EventKind::consumer()`, which is the single place the
partition is written; each indexer derives its `WHERE event_kind = ANY(...)`
filter from it rather than restating a list. `None` means decoded but writing no
derived state: a cursor advances past those on the next event it does fetch.

The discriminants are persisted, so they are append-only: renumbering one
silently relabels every historical row.

`NotePayload` is **not** emitted on the escrow path. A deposit's note reaches
the tree only when the relayer flushes it, so `DepositEscrowed` carries the
commitment and the value-commitment coordinates itself — which is what lets
`fmd-indexer` hold the note pending until the flush that commits it lands.

## Decoding

```rust
let decoded: Vec<DecodedEvent> = chain_types::decode(kind, &topics, &data)?;
```

`decode` takes the stored `EventKind` and the log's `topics` and `data` as
`raw_events` holds them. A payload that does not match the ABI is
`DecodeError::Alloy`.

## Numeric

`numeric` holds `u256_to_bigdecimal` and `bigdecimal_to_u256`. Both directions
live together because they are inverses — the indexers widen on the way in and
the readers narrow on the way out, and a change to one not mirrored in the other
is a silent corruption. `asset-registry` re-exports the narrowing half under its
own error type.

## The `rpc` feature

`rpc::RpcEndpoint` is the JSON-RPC transport every chain-facing service builds
its provider on: a parsed URL, a `reqwest::Client` whose clones share a
connection pool, a caller-supplied request deadline and alloy's retry/backoff
layer.

It is here because it is the chain-facing layer, and off by default because the
indexers link this crate for the ABI alone and must not pull a provider stack in
with it.

The deadline is the point. Alloy's `ProviderBuilder::new().on_http(url)` uses a
default client with **no request timeout**, so a node that accepts the connection
and never answers hangs the caller forever — `tests/rpc_timeout.rs` pins that it
now does not.

## Tests

- `tests/decode_roundtrip.rs` encodes and re-decodes every event.
- `tests/sig_check.rs` pins every event's topic0 against the hash taken from the
  contract's own ABI JSON, not from the `sol!` block it checks. A field that
  drifts from the contract changes the signature hash, the indexer stops
  matching the log, and events vanish from the pipeline without an error; this
  turns that into a test failure. The `interface` declarations are **not**
  covered — no ABI JSON ships for them, so an expected selector could only be
  re-derived from the same signature string `sol!` already compiles.
- `tests/rpc_timeout.rs` *(feature `rpc`)* asserts a call to a node that never
  answers fails on the deadline instead of hanging.

## Layering

May import `shared` and alloy. Must NOT import `database`, `crypto`, or any
binary or service crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
