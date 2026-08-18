# ingester

Multi-chain block/event ingester. Runs database migrations on startup, then
spawns one worker per configured chain. Each worker alternates between chunked
parallel backfill (when far behind tip) and a live tail that follows the head.

## Reorgs

The live tail scans all the way to `tip` — there is **no head buffer**. That
keeps latency at zero and makes reorg detection load-bearing rather than a
backstop, so every tick starts by re-verifying the cursor's anchor.

`chain_state` records the highest block whose hash was verified
(`last_block` + `last_block_hash`). A tick asks the chain for the hash at that
height; if it still matches, nothing has moved and the tick scans forward. If
it does not, the worker walks back through the hashes already stored in
`raw_events` — at most `reorg_depth` blocks — until one still matches, and
rewinds to just above it. Finding nothing that matches rewinds the whole
window.

Detection deliberately looks *backwards* at the anchor rather than forwards at
incoming logs. Comparing incoming logs to stored ones cannot work: a tick only
fetches blocks above `last_scanned_block`, so the stored lookup always misses
and the check always answers "no reorg".

A rewind deletes `raw_events` at or above the fork, resets the cursor, and
records the fork in `chain_reorgs` — all in one transaction.

### Downstream retraction

Consumers stream `raw_events` by ascending `id`. Replacement rows get fresh,
higher ids, so the replay side takes care of itself. State already *derived*
from the deleted rows does not: it sits below the consumer's cursor where
nothing revisits it.

`chain_reorgs` closes that gap. It is durable on purpose — `pg_notify` alone is
fire-and-forget, so a consumer that is down during the fork would never hear
about it. Consumers call `database::reorg::apply_pending`, which drops derived
rows at or above the fork and rewinds their cursor to replay. `consumer_cursors.last_reorg_id`
tracks how far each consumer has processed the log.

A `raw_events_reorg` NOTIFY (`<chain_id>:<rewind_to>`) is also emitted for
consumers that want to react promptly rather than on their next tick.

## Failure handling

- Transient RPC and database errors retry with exponential backoff and jitter.
  After `MAX_CONSECUTIVE_FAILURES` the worker surrenders the chain, which
  releases its advisory lock so a standby can take over.
- The supervisor in `main` restarts a worker that lost its lock or failed, up
  to `MAX_WORKER_RESTARTS`. If every restart is exhausted the process exits
  non-zero — a fully stalled ingester must not look healthy to its
  orchestrator.
- All RPC calls carry request and connect timeouts. Without them a half-open
  socket parks a worker indefinitely while it still holds the chain lock, so no
  standby can take over.
- SIGTERM/ctrl-c drains via `shared::shutdown`, dropping each chain lock
  promptly so a replacement replica picks the chain up straight away.

## Replicas

Safe to run as N replicas, as failover rather than scale-out. Each chain worker
must hold a Postgres advisory lock (`database::advisory`, namespace
`NS_INGESTER`) before ingesting. One replica wins per chain; the rest block in a
retry loop and take over when the leader's lock frees. Throughput per chain stays
1x — the lock serialises, it does not shard.

The lock is held on a dedicated connection, deliberately not one from the bb8
pool: a pooled connection is returned after the query and eventually reaped by
`idle_timeout`, which would silently release the lock while the process kept
writing. The leader also re-checks that connection on an interval and stops the
worker if it has died, so a dropped connection cannot leave two writers on one
chain.

Migrations run under their own advisory lock (`MIGRATE_KEY`), so replicas
booting together serialise instead of racing `diesel_migrations`.

## Run

```bash
INGESTER_CONFIG=ingester.toml cargo run -p ingester
```

Config is validated before any worker spawns, so a bad address or a zero chunk
size fails the process at startup rather than after a standby has waited out a
chain lock.

## Config (`ingester.toml`)

```toml
database_url = "postgres://..."

[[chains]]
chain_id      = 1
rpc_url       = "https://..."
pool_address  = "0x..."
start_block   = 18_000_000
reorg_depth             = 32      # optional
block_poll_ms           = 2000    # optional
backfill_threshold      = 100     # optional, blocks behind tip to trigger backfill mode
backfill_concurrency    = 8       # optional
chunk_blocks            = 50_000  # optional, range size during backfill
meta_concurrency        = 16      # optional
rpc_timeout_ms          = 30_000  # optional
rpc_connect_timeout_ms  = 10_000  # optional
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `database_url` | yes | — | Postgres URL |
| `chains[].chain_id` | yes | — | EVM chain id; must be unique |
| `chains[].rpc_url` | yes | — | HTTP RPC endpoint. Redacted to scheme+host in logs — provider API keys live in the path |
| `chains[].pool_address` | yes | — | Contract address to follow |
| `chains[].start_block` | yes | — | First block to ingest; must be >= 0 |
| `chains[].reorg_depth` | no | 32 | How far back the anchor walk searches, and the backfill's safety margin below tip. Not a live head buffer |
| `chains[].block_poll_ms` | no | 2000 | Tip poll cadence |
| `chains[].backfill_threshold` | no | 100 | Lag (blocks) that flips the worker into backfill mode. Must exceed `reorg_depth`, or the worker oscillates between the two modes |
| `chains[].backfill_concurrency` | no | 8 | Parallel range fetches in backfill |
| `chains[].chunk_blocks` | no | 50000 | Block range per backfill chunk, and the cap on one live tick's span |
| `chains[].meta_concurrency` | no | 16 | Cap on simultaneous `eth_getBlockByNumber` calls |
| `chains[].rpc_timeout_ms` | no | 30000 | Whole-request RPC timeout |
| `chains[].rpc_connect_timeout_ms` | no | 10000 | RPC connect timeout |

Any key can be overridden per chain from the environment:
`INGESTER_CHAIN_<id>_POOL_ADDRESS`, `INGESTER_CHAIN_<id>_RPC_URL`,
`INGESTER_CHAIN_<id>_START_BLOCK`. A malformed `START_BLOCK` fails startup
rather than silently falling back to the TOML value.
