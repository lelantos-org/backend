# database

The Diesel schema, the embedded migrations, the bb8 async pool, and the two
pieces of cross-consumer machinery every indexer shares: the cursor repository
and reorg retraction. No business logic.

## Layout

| Module | Contents |
|--------|----------|
| `schema` | `diesel::table!` definitions (generated; edit via migrations) |
| `models` | Queryable/Insertable structs for the shared tables |
| `pool` | `DbPool`, `DbConn`, `PoolCfg` presets, `build_pool` |
| `migrate` | `MIGRATIONS` (embedded) + `run(database_url)` |
| `cursor` | `CursorRepo` trait + `PostgresCursorRepo` |
| `advisory` | Session-level per-chain advisory locks |
| `reorg` | `apply_pending` — retract derived state after a fork |

## Tables

One Postgres instance backs the whole system. `ingester` is the only writer of
`raw_events`; every other table is derived from it (or, for
`screened_addresses`, populated out of band).

| Table | Written by | Read by |
|-------|-----------|---------|
| `raw_events` | ingester | fmd-indexer, explorer-indexer |
| `chain_state` | ingester | ingester, explorer-webserver (which chains are indexed) |
| `chain_reorgs` | ingester | every consumer, via `reorg::apply_pending` |
| `consumer_cursors` | every indexer | every indexer |
| `notes` | fmd-indexer | fmd-webserver, relayer (tree bootstrap) |
| `spent_nullifiers` | fmd-indexer | fmd-webserver, relayer (nullifier guard) |
| `matches` | fmd-indexer | fmd-webserver |
| `subscriptions` | fmd-webserver | fmd-indexer |
| `assets` | explorer-indexer | explorer-webserver, relayer (`/chains`) |
| `asset_flows` | explorer-indexer | explorer-webserver |
| `tree_advances` | explorer-indexer | explorer-webserver, relayer (tree bootstrap) |
| `deposit_escrowed_events` | explorer-indexer | relayer (flush pipeline) |
| `screened_addresses` | ops SQL / seed migration | risk-webserver |

Three materialized views (`tree_advances_hourly`, `asset_flows_hourly`,
`asset_locked`) are refreshed `CONCURRENTLY` by `explorer-indexer` — see that
crate's [README](../explorer-indexer/README.md).

## Migrations

Embedded at compile time, so a binary carries the schema it was built against.

```sh
just db-shell                        # psql, from stack/
diesel migration generate <name>     # new pair, from crates/database/
```

`migrate::run` is synchronous — call it once at startup from
`tokio::task::spawn_blocking`. Three binaries do: `ingester`, `risk-webserver`
(nothing else creates `screened_addresses`), and `relayer` (compose dependency
graphs can bring it up before the ingester). Migrations are idempotent, so the
overlap is harmless.

`diesel_migrations` takes no lock of its own, so N replicas booting together
can apply the same migration concurrently. `ingester` serialises them under
`advisory::MIGRATE_KEY`; the other two do not.

## Pool presets

`PoolCfg` sizes the pool per workload rather than leaving it to each binary.
All three use a 5 s connection timeout and a 10 min idle timeout.

| Preset | `max_size` | `min_idle` |
|--------|-----------|-----------|
| `webserver()` | 32 | 8 |
| `indexer()` (also `Default`) | 8 | 2 |
| `relayer()` | 4 | 1 |

## Cursors

`consumer_cursors` is one row per `(name, chain_id)`. Do not re-implement it
per crate.

`upsert_monotonic` is the normal batch advance: a write whose `last_event_id` is
not greater than the stored one is a no-op. That guards the read-modify-write in
every tick — two processes that fetched the same cursor would otherwise let the
slower one overwrite the faster one's watermark and re-process an unbounded
range. Plain `upsert` is unconditional and can move a cursor *backwards*; use it
only where a rewind is the intent.

## Advisory locks

`ChainLock::try_acquire` takes a **session-level** `pg_try_advisory_lock` on a
**dedicated connection that never enters the bb8 pool**. Both properties are
load-bearing: the lock has to outlive individual statements (the indexers issue
standalone autocommit statements), and a pooled connection would be returned
after the query and eventually reaped by `idle_timeout` — silently releasing the
lock while the process kept writing. Two writers, no error.

Namespaces are distinct per service so two services can each hold a lock for the
same chain without excluding one another:

| Constant | Owner |
|----------|-------|
| `NS_INGESTER` | ingester's per-chain worker locks |
| `NS_FMD_CONSUME` | fmd-indexer's per-chain consume locks |
| `NS_MIGRATE` / `MIGRATE_KEY` | the single, chain-independent migration lock |

## Reorg retraction

The ingester deletes `raw_events` for blocks a fork took away and re-ingests the
canonical replacements. Those replacements get fresh, higher `BIGSERIAL` ids, so
consumers streaming by ascending `id` re-read them on their own — the replay
side takes care of itself.

What does not is state already *derived* from the deleted rows: it sits below
the consumer's cursor where nothing revisits it. `reorg::apply_pending(pool,
consumer_name, chain_id)` is the other half. In one transaction it deletes
`notes`, `spent_nullifiers`, `tree_advances`, `asset_flows`, and
`deposit_escrowed_events` at or above the fork block, then rewinds that
consumer's cursor. `matches` follows via `ON DELETE CASCADE` from `notes`.

The cursor is rewound to id 0, not to a computed id: once rows have been
re-inserted, `raw_events.id` is no longer ordered by block, so no id cleanly
means "just before this block". Replaying from the start is slower but correct,
and every consumer write is idempotent.

`consumer_cursors.last_reorg_id` records how far each consumer has processed the
log, so the retraction runs once per fork per consumer. Callers today:
`fmd-indexer` and `explorer-indexer`, both at the top of `tick_chain`, before
reading.

## Layering

May import `shared`. Must NOT import any indexer, webserver, relayer, or
service crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
