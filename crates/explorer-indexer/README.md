# explorer-indexer

Flow analytics for explorer-ui. Consumes raw chain events from the database and
projects them into `asset_flows` and `yield_fee_events`, keeping the materialized
views over them current.

**Reads no chain.** Everything needing an RPC — ERC-20 metadata and the
yield-state poll — and every table the wallet or the relayer depends on belongs
to [`protocol-indexer`](../protocol-indexer/README.md). This crate is
database-in, database-out.

That split is by consumer rather than by table name: `tree_advances` and
`deposit_escrowed_events` used to live here and look like analytics, but the
relayer's Merkle bootstrap and flush pipeline read them, so they moved.

Public data only. **Must not depend on `crypto`** — the privacy gate is
recorded in `Cargo.toml` and `lib.rs`, but nothing in CI checks it, so a new
dependency edge has to be caught in review.

## Layering

Standard binary layout (see [ARCHITECTURE.md](../../ARCHITECTURE.md)):

| Layer | What |
|-------|------|
| `app/` | Config and build stamp |
| `adapters/` | `locks`: the per-chain advisory lock that elects one replica per chain |
| `domain/` | Error type |
| `repositories/` | One module per table written: `asset_flows` (plus its view refreshes), `yield_fee_events`. The cursor and `raw_events` reads go through `database` |
| `services/consume/` | The consume tick: `events` routes each decoded event to the row builder for its projection (`flows`, `yield_fees`), `plan` batches the writes, `tick` runs the loop and `refresh` gates the materialized views |

## Run

```bash
EXPLORER_INDEXER_CONFIG=explorer-indexer.toml cargo run -p explorer-indexer
```

## Config (`explorer-indexer.toml`)

```toml
database_url = "postgres://..."
tick_ms = 1000   # optional, default 1000
batch = 500      # optional, default 500
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `database_url` | yes | — | Postgres URL |
| `tick_ms` | no | 1000 | Idle **ceiling** between batches, not a fixed period — see `shared::tick` |
| `batch` | no | 500 | Max events per chain per tick |

There is deliberately no `[[chains]]` block and no env overlay: this binary opens
no RPC connection. It links `alloy` only for the primitive types (`Address`,
`U256`) that arrive from `chain-types`'s decode.

## Loop

One `TickService` over `raw_events`, per chain, from a cursor in
`consumer_cursors` (`name = 'explorer'`), fetching the kinds
`EventKind::kinds_for(Consumer::Explorer)` returns:

| Event | Projected into |
|-------|----------------|
| `AssetMoved` | `asset_flows` |
| `PerfFeeAccrued` / `NormalizedFeeSwept` | `yield_fee_events` |

The fetch filter is *derived* from that partition rather than restated. One
exhaustive match with no wildcard arm owns the assignment, so a new `EventKind`
fails to compile until it is given a consumer — the drift that once left
`asset_yield` permanently empty, because the cursor only advances to the highest
id among the kinds actually fetched.

A tick decodes its whole window into a `CommitPlan` before issuing any write,
then applies it as one batched call per projection — one pool checkout each, and
a single multi-row statement for the append-only tables. Writing per event
instead cost a checkout and a round trip per row, so a 500-event batch was 500
sequential round trips against a pool of eight.

Both tables here are append-only and independent, so unlike protocol-indexer's
plan there is no causal group order to preserve: nothing written here is an
`UPDATE` keyed on a row another group inserts.

There is no enclosing transaction, deliberately, matching `fmd-indexer`: cursor
commit per batch is at-least-once, every write is idempotent via `ON CONFLICT`,
and the cursor moves only once they all succeed, so a crash mid-apply replays the
same window and converges.

⚠️ `asset_flows` rows carry `asset_id_u64` and `token`, and their `assets` row is
now written by a different process. The two no longer land in one tick, so a flow
can briefly precede the asset it names. Nothing joins them at write time; it is a
transient read-side gap.

## Reorgs

Each tick calls `database::reorg::apply_pending(pool, Owner::Explorer, chain_id)`
**before reading**, which drops `asset_flows` and `yield_fee_events` at or above
the fork block and rewinds this consumer's cursor to replay. See
[database](../database/README.md#reorg-retraction).

Retraction is scoped to the caller, and the same `Owner` names both the cursor
row and the table set — so this crate cannot delete rows another consumer owns
and rewind only its own position.

## Materialized views

The explorer's read side never aggregates over raw projections at request time;
it reads these, and this crate is what keeps them current. A failed refresh is
logged and retried rather than failing the batch.

| View | Derived from | Marked dirty when a tick sees | Read by |
|------|--------------|-------------------------------|---------|
| `asset_flows_hourly` | `asset_flows` | `AssetMoved` | `/v1/asset-flows` |
| `asset_locked` | `asset_flows` | `AssetMoved` | `/v1/locked` |

`tree_advances_hourly` is refreshed by protocol-indexer, which writes its base
table: a materialized view has to be rebuilt by whoever writes the rows, because
nothing else knows when they changed.

### When they are rebuilt

Both are whole-table aggregates with no incremental form, so a refresh costs
O(source rows) however few rows the tick just wrote. Refreshing inline on every
qualifying tick made a catch-up run cost O(rows² / batch).

`RefreshGate` instead marks a view dirty and rebuilds it when the source changed
**and** either the consumer has caught up — so a reader should see it now — or
`MIN_REFRESH_INTERVAL` (30 s, matching explorer-webserver's analytic cache TTL)
has passed, so a long backfill still publishes progress rather than going dark.

Steady-state freshness is unchanged: the first non-saturated tick after a change
rebuilds, which is the tick that would have done it before. The gate is shared
across chains, since the views are global — N chains marking one view is one
rebuild, not N. A failed refresh keeps the dirty flag, so the change is not lost,
and backs off for the interval rather than retrying every tick.

`asset_locked` keeps `in_base` and `out_base` apart rather than storing a net
column: the reader subtracts in whatever unit it converts to, and a negative
balance stays traceable to its two halves.
