# fmd-indexer

Fuzzy Message Detection indexer. Two concurrent loops:

- **consume** — drains ingested clue events into the FMD pipeline.
- **filter** — runs FMD detection across `filter_workers` rayon threads in batches of `filter_batch`.

Depends on `fmd-crypto` (private side of the privacy gate).

## Run

```bash
# TOML config
FMD_INDEXER_CONFIG=fmd-indexer.toml cargo run -p fmd-indexer

# Or env-only fallback (if config file absent)
DATABASE_URL=postgres://... cargo run -p fmd-indexer
```

## Config

`fmd-indexer.toml`:

```toml
database_url    = "postgres://..."
filter_workers  = 8       # default: available_parallelism, fallback 4
filter_batch    = 1000    # default 1000
filter_tick_ms  = 500     # default 500
retention_days  = 30      # default 30
```

If the config file does not exist, the binary falls back to env vars (`DATABASE_URL` required) and defaults for the rest.

| Key | Default | Notes |
|-----|---------|-------|
| `database_url` | — | Postgres URL (required) |
| `filter_workers` | cores | Rayon global pool size |
| `filter_batch` | 1000 | Rows per filter pass |
| `filter_tick_ms` | 500 | Loop cadence |
| `retention_days` | 30 | Pruning window |
