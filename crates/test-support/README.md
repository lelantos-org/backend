# test-support

The Postgres harness the workspace's database-backed tests share. A dev-dependency
only — nothing ships it.

Eight crates use it: `ingester`, `fmd-indexer`, `protocol-indexer`, `relayer`,
`explorer-webserver`, `protocol-webserver`, `risk-webserver` and
`integration-tests`.

## What it gives a test

| Function | What it does |
|---|---|
| `db_url()` | Connection string of this process's database, migrated and ready |
| `serial_lock()` | The process-wide guard serialising tests that share that database |
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

## How the database is provided

- **Server.** One Postgres 16 container (the version production runs), shared by
  every test process and found by its `org.lelantos.test-postgres` label. The
  first process starts it; later ones and later runs reuse it. It runs on tmpfs
  with `fsync`, `synchronous_commit` and `full_page_writes` off, since durability
  buys nothing for a database that is thrown away.
- **Template.** The migrated schema is kept as a database named after a hash of
  `crates/database/migrations`, so a new or edited migration builds a new one.
- **Clone.** Each process gets `CREATE DATABASE … TEMPLATE`, a copy of well under
  a second instead of 40-odd migrations, and drops it at exit.

Starting the container, building the template and sweeping leftovers (clones of
killed processes, templates of other migration sets) happen under a file lock in
the temp directory, so concurrent processes never race each other.

Under `just test` (cargo-nextest) every test is its own process, so tests get a
database each and run in parallel. Under plain `cargo test` a binary is one
process, and its tests share one database behind the lock below.

Advisory locks, `LISTEN` and schemas are all scoped to a database, so tests that
exercise leader election or chain locks stay isolated from other processes.

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

- `cargo-nextest` (`brew install cargo-nextest` or `cargo install cargo-nextest`).
- A Docker daemon. testcontainers ignores the docker CLI *context*; the justfile
  exports `DOCKER_HOST` for colima's socket when it exists. Running cargo
  directly on colima needs it set by hand, or every database-backed test panics
  in the harness with `SocketNotFoundError("/var/run/docker.sock")`.

`docker rm -f $(docker ps -aq --filter label=org.lelantos.test-postgres)` resets
the server; the next run recreates it.
