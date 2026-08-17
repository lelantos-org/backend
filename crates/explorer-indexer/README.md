# explorer-indexer

Consumes raw chain events from the database and projects them into explorer-facing tables (assets, tree advances). Public-data only — must not depend on `fmd-crypto` (CI gate enforces).

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
| `tick_ms` | no | 1000 | Poll interval between batches |
| `batch` | no | 500 | Max events per chain per tick |

## Loop

`services::consume::run(ctx)` polls per-chain cursors, decodes `AssetRegistered` / `RootAdvanced` events from `raw_events`, and writes projections via `repositories::{assets,tree_advances}`. Cursor commit per batch is at-least-once; idempotency is enforced by `ON CONFLICT` on each projection table.

## Materialized views

The explorer's read side never aggregates over raw projections at request time; it reads these, and this crate is what keeps them current. Each is refreshed `CONCURRENTLY` at the end of a tick that committed the events it derives from, and a failed refresh is logged and retried on the next such tick rather than failing the batch.

| View | Derived from | Refreshed when a tick sees | Read by |
|------|--------------|----------------------------|---------|
| `tree_advances_hourly` | `tree_advances` | `RootAdvanced` | `/v1/tx-counts`, `/v1/chain-flows-24h` |
| `asset_flows_hourly` | `asset_flows` | `AssetMoved` | `/v1/asset-flows` |
| `asset_locked` | `asset_flows` | `AssetMoved` | `/v1/locked` |

`asset_locked` keeps `in_base` and `out_base` apart rather than storing a net column: the reader subtracts in whatever unit it converts to, and a negative balance stays traceable to its two halves.
