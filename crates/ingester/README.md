# ingester

Multi-chain block/event ingester. Runs database migrations on startup, then spawns one worker per configured chain. Each worker polls its RPC, follows the head with a reorg buffer, and backfills in chunks when far behind tip.

## Run

```bash
INGESTER_CONFIG=ingester.toml cargo run -p ingester
```

Migrations run once at startup against `database_url` before the pool is built.

## Config (`ingester.toml`)

```toml
database_url = "postgres://..."

[[chains]]
chain_id      = 1
rpc_url       = "https://..."
pool_address  = "0x..."
start_block   = 18_000_000
reorg_depth          = 32      # optional
block_poll_ms        = 2000    # optional
backfill_threshold   = 100     # optional, blocks behind tip to trigger backfill mode
backfill_concurrency = 8       # optional
chunk_blocks         = 50_000  # optional, range size during backfill
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `database_url` | yes | — | Postgres URL |
| `chains[].chain_id` | yes | — | EVM chain id |
| `chains[].rpc_url` | yes | — | HTTP RPC endpoint |
| `chains[].pool_address` | yes | — | Contract address to follow |
| `chains[].start_block` | yes | — | First block to ingest |
| `chains[].reorg_depth` | no | 32 | Head buffer depth |
| `chains[].block_poll_ms` | no | 2000 | Tip poll cadence |
| `chains[].backfill_threshold` | no | 100 | Lag (blocks) that flips worker to backfill mode |
| `chains[].backfill_concurrency` | no | 8 | Parallel range fetches in backfill |
| `chains[].chunk_blocks` | no | 50000 | Block range per backfill chunk |
