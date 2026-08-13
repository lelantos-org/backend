# fmd-indexer

Fuzzy Message Detection indexer. Two concurrent loops:

- **consume** — drains ingested clue events into the FMD pipeline.
- **filter** — runs FMD detection across `filter_workers` rayon threads in batches of `filter_batch`.

Depends on `fmd-crypto` (private side of the privacy gate).

## Replicas

Safe to run as N replicas, as **failover, not scale-out**. Each consume tick must
hold a Postgres advisory lock for the chain (`adapters::locks::ChainLocks`,
namespace `database::advisory::NS_FMD_CONSUME`). One replica wins and works; the
rest skip their tick and retry. Throughput per chain stays 1x no matter how many
replicas run — the lock serialises, it does not shard.

Failover is automatic: the lock lives on a dedicated connection owned by the
leader, so when that process exits or dies the lock releases and a standby is
promoted on its next tick (<= `filter_tick_ms`). The leader also re-checks its own
lock connection each tick and steps down if it has died, so a silently dropped
connection cannot produce two writers.

The lock is what makes concurrent writes safe. Without it, `spent_nullifiers.seq`
is assigned by a read-then-write with no transaction and develops permanent silent
gaps under a second writer, which misaligns the `/v1/chains/{id}/nullifiers/chunks/*`
feed. `ChainLocks::disabled()` exists only for single-process tests.

The filter loop is intentionally unlocked — `matches` inserts are idempotent, and
its cursor advance is monotonic.

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
```

If the config file does not exist, the binary falls back to env vars (`DATABASE_URL` required) and defaults for the rest.

| Key | Default | Notes |
|-----|---------|-------|
| `database_url` | — | Postgres URL (required) |
| `filter_workers` | cores | Rayon global pool size |
| `filter_batch` | 1000 | Rows per filter pass |
| `filter_tick_ms` | 500 | Loop cadence |

## Retention

There is none. `raw_events`, `notes`, `matches`, and `subscriptions` grow
without bound. A `retention_days` key used to be documented here as a
"pruning window", but nothing ever read it — no operator should have been
relying on data ageing out, so the dead key was removed rather than left
implying a policy that did not exist.

Adding real pruning needs a decision this crate cannot make alone:
`raw_events` is shared with `explorer-indexer`, so trimming it has to respect
the slowest consumer's cursor, and `matches` retention is a privacy question
(the table is a user → note index) rather than a disk-space one.
