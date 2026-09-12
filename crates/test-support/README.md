# test-support

The Postgres harness the workspace's database-backed tests share. A dev-dependency
only — nothing ships it.

Seven crates use it: `ingester`, `fmd-indexer`, `protocol-indexer`, `relayer`,
`registry-webserver`, `risk-webserver` and `integration-tests`.

## What it gives a test

| Function | What it does |
|---|---|
| `db_url()` | Connection string of this binary's container, migrated and ready |
| `serial_lock()` | The process-wide guard serialising tests that share one container |
| `fresh_pool(cfg, tables)` | A pool over an empty database, plus that guard |

And, in `fixtures`, the seeding a replay test needs:

| Function | What it does |
|---|---|
| `insert_chain_state(pool, chain_id)` | Marks a chain scanned from genesis, so a consume tick has something to own |
| `build_log(data, block_n, block_ts, tx_byte, log_idx)` | One synthetic log, filled in as a node would report it |
| `insert_log(pool, chain_id, log, kind)` | Stores that log in `raw_events` as the ingester would |
| `pool_addr()` / `POOL_ADDR` | The fixed MASP address the synthetic logs are emitted from |

These were copied verbatim into every replay test before they lived here. Each
test still owns its own `TABLES` list: the set a binary writes is a property of
that binary, and truncating a table it never touches would couple unrelated
suites.

Tests run against a real schema rather than a mock, so a migration that breaks a
query fails here rather than in production.

## One container per test binary

Started lazily on first use and held for the process lifetime, so the cost is
paid once per binary rather than once per test. `testcontainers` returns from
`start()` before Postgres accepts connections, so the harness polls for
readiness itself before running migrations.

## Why the lock

`fresh_pool` truncates with `RESTART IDENTITY CASCADE` — ids stay comparable
across tests, which is what lets a test assert on `id = 1`. Two tests running
concurrently against the one shared container would therefore clear each other's
rows mid-assertion.

`serial_lock` is what prevents that, and it is returned as an `OwnedMutexGuard`
rather than taken internally: the guard must outlive every use of the pool, so
handing it back makes the test's own binding decide the critical section. Drop it
early and the truncation of the next test can land underneath this one.

## Requirements

A working Docker daemon. On a machine running colima rather than Docker Desktop,
`testcontainers` does not read the docker CLI's *context* and falls back to
`/var/run/docker.sock`, so `DOCKER_HOST` has to be set explicitly:

```sh
DOCKER_HOST=unix:///Users/you/.colima/default/docker.sock cargo test --workspace
```

Without it every database-backed test panics in the harness with
`SocketNotFoundError("/var/run/docker.sock")`, before reaching an assertion —
which reads like a code regression and is not one.
