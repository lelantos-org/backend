# Lelantos Backend

Rust workspace of backend services for Lelantos: EVM log ingestion, FMD (fuzzy message detection) indexing, explorer APIs, tree-advance relaying, and quote aggregation for shielded swaps.

See [ARCHITECTURE.md](ARCHITECTURE.md) for layering rules and conventions.

## System overview

Everything hangs off one Postgres. The `ingester` is the only writer of raw EVM
logs; every other indexer consumes them through a cursor and writes its own
derived tables, which the webservers read.

```mermaid
flowchart LR
  EVM[(EVM chains)] -->|eth_getLogs| ING[ingester]
  ING --> RAW[(raw_events)]
  RAW --> FI[fmd-indexer]
  RAW --> EI[explorer-indexer]
  FI --> FT[(notes / matches<br/>spent_nullifiers)]
  EI --> ET[(assets / asset_flows<br/>tree_advances)]
  FT --> FW[fmd-webserver]
  ET --> EW[explorer-webserver]
  REL[relayer] -->|prove + submit tx| EVM
  FT -->|spent nullifiers| REL
  MQ[metaquoter] -->|eth_call quoters| EVM
  FW --> APP[clients]
  EW --> APP
  REL --> APP
  MQ --> APP
```

## Services

### Ingestion — `ingester`

One worker per configured chain. Live tail polls the tip, fetches logs since
the cursor, detects reorgs by comparing block hashes, and commits rows to
`raw_events` + `chain_state`. Backfill walks history in ranges through the same
ingest path. Decoding is delegated to `chain-types`.

```mermaid
flowchart TD
  subgraph worker["worker (per chain)"]
    LIVE[live tick] --> RPC[HttpRpc adapter]
    RPC --> DEC[decode: logs_to_rows]
    DEC --> ING[IngestService]
    LIVE --> REORG[ReorgService]
    BF[BackfillService] --> ING
  end
  ING --> DB[(raw_events<br/>chain_state)]
  REORG -->|hash mismatch| DB
```

### FMD — `fmd-indexer` + `fmd-webserver`

**Indexer.** Two tick loops over `raw_events`. *Consume* decodes
`IntentEscrowed` and nullifier events into `notes` and `spent_nullifiers`,
holding escrowed intents in a pending map until the batch that commits them
lands. *Filter* runs FMD detection: each note's clue is tested against every
active subscription key (`fmd-crypto`), producing `matches`. Both keep their own
row in `consumer_cursors`; filter also backfills new subscriptions over
historical notes. With the `parallel` feature the clue tests fan out over rayon.

**Webserver.** Read-only axum API over the same tables. Clients register a
detection key and get a capability token back; matches and note payloads are
fetched with it. Commitment and nullifier chunks are served as fixed-size pages
so a client can sync the tree locally. Cache-control is per route — token-keyed
routes are `no-store`, public ones are short-TTL.

```mermaid
flowchart LR
  subgraph idx["fmd-indexer"]
    C[ConsumeService]
    F[FilterService]
  end
  RAW[(raw_events)] --> C
  C --> N[(notes)]
  C --> SN[(spent_nullifiers)]
  N --> F
  SUB[(subscriptions)] --> F
  F -->|clue vs detection key| M[(matches)]
  C -.cursor.-> CUR[(consumer_cursors)]
  F -.cursor.-> CUR
  subgraph web["fmd-webserver"]
    R["/v1/subscriptions · /v1/matches · /v1/notes<br/>/v1/chains/:id/commitments/chunks/:n<br/>/v1/chains/:id/nullifiers/chunks/:n<br/>/v1/tree-state"]
  end
  N --> R
  SN --> R
  M --> R
  SUB --> R
  R --> CL[client]
```

### Explorer — `explorer-indexer` + `explorer-webserver`

**Indexer.** Single tick service, one pass per chain. Reads `raw_events` from
its cursor and projects public analytics tables: asset registrations, per-tx
in/out amounts, and tree-advance records (old root, new root, leaves inserted).

**Webserver.** Read-only axum API over those tables, with an in-process TTL
cache in front of the analytic aggregates.

```mermaid
flowchart LR
  RAW[(raw_events)] --> T["explorer-indexer<br/>ConsumeService tick"]
  T --> A[(assets)]
  T --> AF[(asset_flows)]
  T --> TA[(tree_advances)]
  T -.cursor.-> CUR[(consumer_cursors)]
  subgraph web["explorer-webserver"]
    RT["/v1/assets · /v1/tree-advances<br/>/v1/tx-counts · /v1/chain-flows-24h<br/>/v1/asset-flows"]
    CA[AppCache TTL]
  end
  A --> CA
  AF --> CA
  TA --> CA
  CA --> RT
  RT --> CL[client]
```

### Relayer — `relayer`

The only service that writes on-chain. Clients POST a spend or swap payload; a
per-chain pipeline builds the witness against a mirrored copy of the commitment
tree, generates a Groth16 proof (ark-circom, serialized behind a mutex since it
is CPU-bound), and submits the batch transaction. Nullifier guard rejects
double-spends before proving, the oracle and gas estimator price the fee, and
successful flushes publish intent-lifecycle events on an SSE stream. The
`/estimate` routes run the same build without submitting.

```mermaid
flowchart TD
  CL[client] -->|POST /v1/spend, /v1/swap| PIPE[pipeline]
  PIPE --> NG[nullifier guard]
  NG --> W[witness builder]
  MIR[tree mirror] --> W
  W --> PR[ArkCircomProver Groth16]
  PR --> SUB[submitter]
  SUB -->|flushBatch tx| EVM[(EVM)]
  PIPE --> FQ[fee quote]
  FQ --> OR[oracle] & GE[gas estimator]
  SUB --> EV[event broadcaster]
  EV -->|SSE /v1/intents/stream| CL
  CL -->|POST /v1/*/estimate| FQ
```

### Metaquoter — `metaquoter`

DB-less quote aggregator. One POST gets raced against every quoter that
supports the requested chain (Uniswap V3 via `eth_call` today), each with its
own deadline; the best `expected_out` wins and slow quoters are dropped rather
than failing the request.

```mermaid
flowchart LR
  CL[client] -->|POST /v1/quotes| QS[RacingQuoteService]
  QS --> Q1[UniV3Quoter]
  QS --> Q2[...other quoters]
  Q1 -->|eth_call| RPC[(EVM RPC)]
  Q2 --> RPC
  Q1 & Q2 --> BEST[max expected_out<br/>per-quoter deadline]
  BEST --> CL
```

### Libraries

No IO loop of their own; every binary sits on top of them. `shared` holds
entities, shutdown, the tick driver, config loading and tracing init;
`chain-types` the ABI types and decoding; `fmd-crypto` the Poseidon / Baby
Jubjub / filter / tree primitives; `database` the Diesel schema, migrations,
bb8 pool and cursor repository. `integration-tests` drives the whole stack
end-to-end via testcontainers.

```mermaid
flowchart BT
  BINS["ingester · fmd-indexer · explorer-indexer<br/>fmd-webserver · explorer-webserver<br/>relayer · metaquoter"] --> SH[shared]
  BINS --> CT[chain-types]
  BINS --> FC[fmd-crypto]
  BINS --> DB[database]
```

## Requirements

- Rust 1.95 (pinned via `rust-toolchain.toml`)
- [`just`](https://github.com/casey/just)
- Docker (local stack, integration tests)

## Development

```sh
just build    # cargo build --workspace
just test     # cargo test --workspace
just ci       # fmt + clippy + test
```

## Local stack

`stack/` runs the full system with Docker Compose: Postgres, an ephemeral Anvil node, contract deployment, and all services.

```sh
cd stack
just up               # everything (profile=all)
just up-profile fmd   # single profile: db | anvil | ingester | fmd | explorer | relayer | metaquoter
just logs <service>   # tail one service
just down             # stop + wipe volumes
```

The relayer needs circuit artifacts; `just up` fetches them automatically (`just fetch-circuits` to run manually). Service configs live in `stack/deploy/*.toml`.

## License

MIT OR Apache-2.0
