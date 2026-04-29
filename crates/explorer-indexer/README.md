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
