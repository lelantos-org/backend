# protocol-indexer

Consumes raw chain events from the database and projects them into the state the
**wallet and the relayer** depend on: the asset catalog, yield bindings and their
polled index, the Merkle advance log, and the deposit ledger the relayer flushes
from.

Split out of `explorer-indexer`. The dividing line is not "explorer versus
protocol tables" but **who reads them**: `tree_advances` and
`deposit_escrowed_events` sit in the explorer's old projection set yet feed the
relayer's on-chain write path, so classing them as analytics would have put that
path behind a service allowed to lag.

Public data only. **Must not depend on `common-crypto`** — the privacy gate is
recorded in `Cargo.toml` and `lib.rs`, but nothing in CI checks it, so a new
dependency edge has to be caught in review.

## Run

```bash
PROTOCOL_INDEXER_CONFIG=protocol-indexer.toml cargo run -p protocol-indexer
```

## Config (`protocol-indexer.toml`)

```toml
database_url = "postgres://..."
tick_ms = 1000   # optional, default 1000
batch = 500      # optional, default 500

[[chains]]
chain_id = 31337
rpc_url  = "http://anvil:8545"
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `database_url` | yes | — | Postgres URL |
| `tick_ms` | no | 1000 | Idle **ceiling** between batches, not a fixed period — see `shared::tick` |
| `batch` | no | 500 | Max events per chain per tick |
| `chains[].chain_id` | — | — | Chain to resolve metadata and poll yield state for |
| `chains[].rpc_url` | — | — | HTTP RPC for `decimals()` / `symbol()` and `yieldState` |

Per-chain env overlay: `PROTOCOL_INDEXER_CHAIN_<id>_RPC_URL`.

⚠️ The overlay only rewrites chains **already declared** in the TOML. A variable
naming a chain with no `[[chains]]` block is silently discarded.

⚠️ A chain with no `[[chains]]` block still indexes events, but loses **both**
RPC-backed paths. Its assets keep `decimals = NULL`, and — less obviously — it is
never polled for yield state, so `asset_yield.index_ray` stays NULL,
`/v1/assets` publishes no yield, no APY is measured, and the wallet's
"of which earned" column has nothing to read. Configure every chain carrying a
yield asset.

## Loop

Two `TickService`s share one process and one pool.

**Consume** reads `raw_events` per chain from a cursor in `consumer_cursors`
(`name = 'protocol'`), fetching the kinds `EventKind::kinds_for(Consumer::Protocol)`
returns:

| Event | Projected into |
|-------|----------------|
| `AssetRegistered`, `AssetFeeSet` | `assets` |
| `YieldAssetAdded`, `YieldParamsSet`, `HaltedSet` | `asset_yield` (event columns) |
| `RootAdvanced` | `tree_advances` |
| `DepositEscrowed` / `DepositFlushed` / `DepositCanceled` | `deposit_escrowed_events` |

The fetch filter is *derived* from that partition rather than restated. One
exhaustive match with no wildcard arm owns the assignment, so a new `EventKind`
fails to compile until it is given a consumer — the drift that once left
`asset_yield` permanently empty, because the cursor only advances to the highest
id among the kinds actually fetched.

**Yield state** polls `MASP.yieldState` and writes `asset_yield`'s remaining
columns. It reads no `raw_events` and holds no cursor: the index moves with the
venue's own accounting on every block and emits no log, so there is nothing to
consume. It shares `asset_yield` with the consume loop, disjointly by column, and
the row must exist — created by `YieldAssetAdded` — before the poller's `UPDATE`
matches. Both live here precisely so that ordering stays inside one process.

A tick decodes its whole window into a `CommitPlan` before issuing any write,
then applies it as one batched call per projection. The plan is applied in a
fixed causal order — an asset registered before its rate is set, a venue bound
before its parameters change, a deposit escrowed before it is flushed or
canceled — and rows keep their event order within each group, so repeated events
for one key still resolve last-write-wins.

There is no enclosing transaction, deliberately, matching `fmd-indexer`: cursor
commit per batch is at-least-once, every write is idempotent via `ON CONFLICT`,
and the cursor moves only once they all succeed, so a crash mid-apply replays the
same window and converges.

### ERC-20 metadata

`AssetRegistered` carries neither `decimals` nor `symbol`, so they are swept for
outside the event path: an inline RPC read would let a flaky endpoint stall event
consumption or drop the values permanently. Each column is fetched only when
absent and written as a partial `AsChangeset`, so a token whose `symbol()`
reverts — legal in ERC-20 — still gets its decimals, and neither read clears the
other's stored value.

## Reorgs

Each consume tick calls `database::reorg::apply_pending(pool, Owner::Protocol,
chain_id)` **before reading**, which drops `tree_advances` and
`deposit_escrowed_events` at or above the fork block and rewinds this consumer's
cursor to replay. See [database](../database/README.md#reorg-retraction).

Retraction is scoped to the caller, and the same `Owner` names both the cursor
row and the table set — so a consumer cannot commit under one name while
deleting another's rows. This is what makes three consumers safe: unscoped, the
explorer's retraction would empty `tree_advances` and `deposit_escrowed_events`
while rewinding only its own cursor, stalling the relayer's flush pipeline until
this crate independently reached the same reorg record.

`assets` and `asset_yield` are not retracted. A registration is an idempotent
fact about a token rather than a per-block observation, and `asset_yield` holds
current state that self-heals: the polled columns are overwritten on the next
tick, the event-sourced ones on the cursor rewind.

## Materialized views

| View | Derived from | Marked dirty when a tick sees | Read by |
|------|--------------|-------------------------------|---------|
| `tree_advances_hourly` | `tree_advances` | `RootAdvanced` | `/v1/tx-counts`, `/v1/chain-flows-24h` |

Refreshed here although explorer-webserver is what reads it: a materialized view
has to be rebuilt by whoever writes its base table, because nothing else knows
when the rows changed. `RefreshGate` marks it dirty and rebuilds when the source
changed **and** either the consumer has caught up or `MIN_REFRESH_INTERVAL` (30 s)
has passed, so a long backfill still publishes progress rather than going dark.
A failed refresh keeps the dirty flag and backs off rather than retrying every
tick.
