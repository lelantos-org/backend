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
both its cursor advance and the per-subscription backfill pointer are monotonic.

Each chain a replica leads costs one **non-pooled** Postgres connection (the lock
has to live on a session that is never returned to the pool), so a replica's
connection count is `PoolCfg::indexer()` plus the number of chains it leads.

### The backfill watermark

`notes.id` comes from a sequence, so it is allocated before commit and ids do not
become visible in id order. The per-chain forward pass is unaffected — the
consume lock means one writer per chain, so a chain's own rows always appear in
order — but the backfill walks a single *global* `notes.id` pointer, and two
replicas leading two chains do interleave. Reading the head straight from
`max(id)` therefore steps over a row that commits a moment later, and the
pointer only moves forward, so that note is never scanned for that subscription.

The backfill head is instead an id that was already visible `BACKFILL_LAG`
(5s) ago, which bounds the hazard by how long a single `INSERT` can stay
uncommitted rather than by id ordering. The cost is that a new subscription's
history walk starts one lag late; forward detection is not delayed.

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
filter_tick_ms  = 500     # default 500 (idle ceiling, not a fixed period)
consume_batch   = 1000    # default: filter_batch
consume_tick_ms = 500     # default: filter_tick_ms (idle ceiling)
```

If the config file does not exist, the binary falls back to env vars (`DATABASE_URL` required) and defaults for the rest.

| Key | Default | Notes |
|-----|---------|-------|
| `database_url` | — | Postgres URL (required) |
| `filter_workers` | cores | Rayon global pool size |
| `filter_batch` | 1000 | Rows per filter pass |
| `filter_tick_ms` | 500 | Filter loop idle **ceiling** (see below) |
| `consume_batch` | `filter_batch` | Raw events per consume pass |
| `consume_tick_ms` | `filter_tick_ms` | Consume loop idle **ceiling** (see below) |

### Tick cadence

`*_tick_ms` bounds how long an **idle** loop waits, not how often a busy one
runs. Each tick reports `shared::tick::TickProgress`, and the driver sleeps
only when there is nothing left to do:

- `Saturated` (batch came back full) — no sleep at all; the loop goes straight
  round. Initial sync is limited by Postgres, not by the tick.
- `Partial` (advanced, queue drained) — sleep, but from the 50 ms floor, so an
  event landing just after a tick is picked up in ~50 ms.
- `Idle` (cursor did not move) — sleep, doubling up to `*_tick_ms`.

A chain wedged on a too-wide tx reports `Idle`, so a stall backs off instead of
spinning.

`consume_batch` is not only a throughput knob. A tx is committable only once all
of its events fit inside one window, so the batch also bounds the widest tx the
consume loop can handle. When a full window yields nothing the loop widens it
(up to 16x) and retries rather than parking on a tx it can never complete; if
that still yields nothing for `STALL_TICKS` consecutive ticks it logs at
`error!` with the stuck cursor.

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

## Reorgs

Handled, but not here. `ingester` records each fork in `chain_reorgs`; the
consume tick calls `database::reorg::apply_pending` **before reading**, which
drops `notes` and `spent_nullifiers` at or above the fork block — `matches`
follows by `ON DELETE CASCADE` from `notes` — and rewinds the cursor so the
replay rebuilds them. See [database](../database/README.md#reorg-retraction).

Retraction rewinds the cursor, so the tick reports `Saturated` and comes
straight back rather than sleeping on work it has just queued.

The filter loop needs nothing of its own: retracted notes are re-inserted with
fresh, higher ids, which puts them back above the filter cursor.

`NotesRepo::delete_from_block` and `SpentNullifiersRepo::delete_from_block`
implement the same rewind per repository, and predate the shared path. Nothing
in the binary calls them today — only tests do.

## Known gaps

- **`ChainLocks::is_leader` holds its mutex across a TCP connect.** One slow
  `ChainLock::try_acquire` delays the leadership check for every other chain in
  the same replica.
