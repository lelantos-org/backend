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
| `raw_events` | ingester | fmd-indexer, protocol-indexer, explorer-indexer |
| `chain_state` | ingester | ingester, explorer-webserver (which chains are indexed) |
| `chain_reorgs` | ingester | every consumer, via `reorg::apply_pending` |
| `consumer_cursors` | every indexer | every indexer |
| `notes` | fmd-indexer | fmd-webserver, relayer (tree bootstrap) |
| `spent_nullifiers` | fmd-indexer | fmd-webserver, relayer (nullifier guard) |
| `matches` | fmd-indexer | fmd-webserver |
| `subscriptions` | fmd-webserver | fmd-indexer |
| `tree_state` | fmd-indexer | fmd-webserver, relayer (mirror bootstrap) |
| `assets` | protocol-indexer | explorer-webserver, registry-webserver, relayer (`/chains`) — the last two via `asset-registry` |
| `asset_yield` | protocol-indexer, **plus** registry-webserver for the three estimate columns | same readers as `assets` |
| `asset_yield_sample` | registry-webserver (venue-APY worker) | registry-webserver (rate estimate, and the index history `/v1/yield-index` serves) |
| `asset_flows` | explorer-indexer | explorer-webserver |
| `yield_fee_events` | explorer-indexer | nothing yet (retracted on reorg; written ahead of a reader) |
| `tree_advances` | protocol-indexer | explorer-webserver, relayer (tree bootstrap) |
| `deposit_escrowed_events` | protocol-indexer | relayer (flush pipeline) |
| `screened_addresses` | ops SQL / seed migration | risk-webserver |

`asset_yield` is the one table with two writers, and they are disjoint by column:
protocol-indexer creates the row from `YieldAssetAdded` and polls the on-chain
state columns, while registry-webserver's elected measurer writes only the rate
estimate onto a row that already exists.

Note which crate owns what, because the names mislead. `tree_advances` and
`deposit_escrowed_events` read like explorer projections and are not: the relayer
bootstraps its Merkle mirror from the first and drains the second in its flush
pipeline, so both belong to protocol-indexer. The tables are named after the
events they record, not after any consumer — `deposit_escrowed_events` holds
`DepositEscrowed`, whoever reads it.

Three materialized views (`tree_advances_hourly`, `asset_flows_hourly`,
`asset_locked`) are refreshed `CONCURRENTLY` by whichever crate writes their
base table: `tree_advances_hourly` by
[protocol-indexer](../protocol-indexer/README.md), the other two by
[explorer-indexer](../explorer-indexer/README.md). A view has to be rebuilt by
the writer, because nothing else knows when the rows changed.

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

| Preset | `max_size` | `min_idle` | `statement_timeout` |
|--------|-----------|-----------|---------------------|
| `webserver()` | 32 | 8 | 15 s |
| `indexer()` (also `Default`) | 8 | 2 | 300 s |
| `relayer()` | 4 | 1 | 30 s |

`statement_timeout` is the ceiling on how long one query may hold a pooled
connection. Without it a single pathological statement occupies its slot until
the client goes away, and enough of them exhaust `max_size` — every other caller
then fails at checkout after `connection_timeout`, so one slow endpoint takes
the cheap ones and `/health` down with it.

The webserver's is set below a caller's patience, since a response slower than
that is of no use to whoever asked for it. The indexer's is far looser: its
slowest legitimate statement is a `REFRESH MATERIALIZED VIEW CONCURRENTLY` that
grows with the table and that nobody is waiting on, so the deadline is there to
release a connection wedged on a dead socket rather than to bound honest work.

It is applied by a bb8 `CustomizeConnection` hook, once per physical connection
rather than per checkout. ⚠️ That makes it session state, so the caveat in
`direct.rs` applies: a transaction pooler multiplexes clients onto shared server
connections and a `SET` will not follow this client. A deployment running pooled
traffic through PgDog must configure the timeout on the pooler; this then covers
the direct connections only.

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
owner, chain_id)` is the other half. In one transaction it deletes the tables
`owner` writes, at or above the fork block, then rewinds that owner's cursor.

| `Owner` | Cursor name | Retracts |
|---------|-------------|----------|
| `Fmd` | `fmd` | `notes`, `spent_nullifiers`, `tree_state` |
| `Protocol` | `protocol` | `tree_advances`, `deposit_escrowed_events` |
| `Explorer` | `explorer` | `asset_flows`, `yield_fee_events` |

`matches` follows via `ON DELETE CASCADE` from `notes`.

One enum names both the cursor row and the table set, so a consumer cannot commit
under one name while deleting another's rows. That pairing is the invariant: a
table that gets deleted must belong to the owner whose cursor is being rewound,
or the rows are never rebuilt. Retraction was once unscoped — every caller
deleted every derived table — which worked only because each consumer
independently reached the same reorg record and replayed. With three consumers
that window matters: `tree_advances` and `deposit_escrowed_events` are on the
relayer's write path, so an explorer retraction emptying them would stall the
flush pipeline until protocol-indexer happened to catch up.

`assets` and `asset_yield` belong to no owner here. They hold current state
rather than per-block rows and self-heal: the polled columns are overwritten on
the next tick, the event-sourced ones on the cursor rewind.

`tree_state` is the exception to "at or above the fork block": it holds one
current row per chain rather than per-block rows, and its frontier commits to
leaves the deletions above are removing, so the whole row goes. fmd-indexer folds
it back during the replay.

The cursor is rewound to id 0, not to a computed id: once rows have been
re-inserted, `raw_events.id` is no longer ordered by block, so no id cleanly
means "just before this block". Replaying from the start is slower but correct,
and every consumer write is idempotent — `tree_state` included, via the leaf-count
guard on its upsert.

`consumer_cursors.last_reorg_id` records how far each consumer has processed the
log, so the retraction runs once per fork per consumer. Callers today:
`fmd-indexer`, `protocol-indexer` and `explorer-indexer`, each at the top of `tick_chain`, before
reading.

## Layering

May import `shared`. Must NOT import any indexer, webserver, relayer, or
service crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
