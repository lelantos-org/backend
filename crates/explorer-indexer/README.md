# explorer-indexer

Consumes raw chain events from the database and projects them into
explorer-facing tables: assets, per-tx asset flows, tree advances, and the
deposit ledger the relayer flushes from.

Public data only. **Must not depend on `fmd-crypto`** — the privacy gate is
recorded in `Cargo.toml` and `lib.rs`, but nothing in CI checks it, so a new
dependency edge has to be caught in review.

## Run

```bash
EXPLORER_INDEXER_CONFIG=explorer-indexer.toml cargo run -p explorer-indexer
```

## Config (`explorer-indexer.toml`)

```toml
database_url = "postgres://..."
tick_ms = 1000   # optional, default 1000
batch = 500      # optional, default 500

# Optional. Used only to read ERC20 `decimals()` / `symbol()` for registered
# assets — a chain absent here still indexes.
[[chains]]
chain_id = 31337
rpc_url  = "http://anvil:8545"
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `database_url` | yes | — | Postgres URL |
| `tick_ms` | no | 1000 | Idle **ceiling** between batches, not a fixed period — see `shared::tick` |
| `batch` | no | 500 | Max events per chain per tick |
| `chains[].chain_id` | — | — | Chain whose ERC20 metadata should be resolved |
| `chains[].rpc_url` | — | — | HTTP RPC used for `decimals()` / `symbol()` only |

A chain with no `[[chains]]` entry still indexes; its assets keep
`decimals = NULL` and render no human amount. Metadata is written as a partial
`AsChangeset`, so a token whose `symbol()` reverts — legal in ERC-20 — does not
erase decimals already read.

Per-chain env overlay: `EXPLORER_INDEXER_CHAIN_<id>_RPC_URL`.

⚠️ The overlay only rewrites chains **already declared** in the TOML. A variable
naming a chain with no `[[chains]]` block is silently discarded.

## Loop

One `TickService` over `raw_events`, per chain, from a cursor in
`consumer_cursors`. It consumes six event kinds:

| Event | Projected into |
|-------|----------------|
| `AssetRegistered` | `assets` |
| `AssetMoved` | `asset_flows` |
| `RootAdvanced` | `tree_advances` |
| `DepositEscrowed` / `DepositFlushed` / `DepositCanceled` | `deposit_escrowed_events` |

`deposit_escrowed_events` is the canonical deposit ledger, and the relayer's
flush worker re-reads it every tick to find deposits that are neither flushed
nor canceled — so this crate feeds a service that writes on chain, not just the
explorer API.

Cursor commit per batch is at-least-once; idempotency is enforced by
`ON CONFLICT` on each projection table.

## Reorgs

Each tick calls `database::reorg::apply_pending` **before reading**, which drops
`tree_advances`, `asset_flows`, and `deposit_escrowed_events` at or above the
fork block and rewinds the cursor to replay. See
[database](../database/README.md#reorg-retraction).

`assets` is not retracted: a registration is an idempotent fact about a token,
not a per-block observation.

## Materialized views

The explorer's read side never aggregates over raw projections at request time; it reads these, and this crate is what keeps them current. Each is refreshed `CONCURRENTLY` at the end of a tick that committed the events it derives from, and a failed refresh is logged and retried on the next such tick rather than failing the batch.

| View | Derived from | Refreshed when a tick sees | Read by |
|------|--------------|----------------------------|---------|
| `tree_advances_hourly` | `tree_advances` | `RootAdvanced` | `/v1/tx-counts`, `/v1/chain-flows-24h` |
| `asset_flows_hourly` | `asset_flows` | `AssetMoved` | `/v1/asset-flows` |
| `asset_locked` | `asset_flows` | `AssetMoved` | `/v1/locked` |

`asset_locked` keeps `in_base` and `out_base` apart rather than storing a net column: the reader subtracts in whatever unit it converts to, and a negative balance stays traceable to its two halves.
