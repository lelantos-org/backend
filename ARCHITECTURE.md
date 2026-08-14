# Backend Architecture

## Crate map

```
shared            primitives: entities, shutdown, tick driver, config loader, tracing init
chain-types       ABI types + decode (no DB, no IO)
fmd-crypto        FMD primitives (poseidon, baby-jubjub, filter, tree)
database          diesel schema, migrations, bb8 pool, CursorRepo
webserver-shared  HTTP error type shared by both webservers

ingester          live + backfill log ingester (per chain)
fmd-indexer       FMD consume + filter
explorer-indexer  asset / tree-advance indexer
fmd-webserver     FMD HTTP API (notes, matches, subscriptions, tree)
explorer-webserver explorer HTTP API (assets, flows, tree-advances)
risk-webserver    read-only address screening API (banned / high-risk lookup)
relayer           tree-advance prover + submitter

integration-tests cross-crate end-to-end via testcontainers
```

## Layering rules

Within a binary crate the layers are:

1. `app/` — config, app state, version. No business logic.
2. `adapters/` — talks to external systems (RPC, file, …). No domain types.
3. `domain/` — pure types + decode/transform helpers. No IO, no DB.
4. `repositories/` — database I/O for one aggregate. Returns rows.
5. `services/` — orchestration. Depends on repos + adapters. Trait + Impl.
6. `handlers/` — thin entry points (axum routes, worker tick loops).
7. `main.rs` — wire deps, spawn handlers, watch shutdown.

**Forbidden imports**

- `domain` → `repositories`, `adapters`, `services`, `handlers`, `app`.
- `repositories` → `services`, `handlers`, `adapters`.
- `services` → `handlers`. (Service-internal `events.rs` modules are NOT the same as the `handlers/` layer.)
- Any binary → another binary.
- `database` → any binary or service crate.
- `shared` → anything internal (it is the bottom of the stack).

## Conventions

- **Service contract**: every long-running indexer service implements `shared::tick::TickService`. Binary main wires it through `shared::tick::run`. Existing examples: `fmd_indexer::services::ConsumeServiceImpl`, `fmd_indexer::services::FilterServiceImpl`, `explorer_indexer::services::consume::ConsumeServiceImpl`.
- **Repos**: trait + `Postgres*Repo` impl, owned via `Arc<dyn Repo>`. The shared cursor lives in `database::CursorRepo` — do not re-implement it.
- **Shutdown**: every binary uses `shared::shutdown::channel()` + `watch_signals(trigger)`.
- **Config**: TOML-loaded binaries call `shared::config::load_toml(env_var, default_path)`.
- **Webserver errors**: `webserver_shared::AppError` + `AppResult` are the canonical types. Crate-local `error.rs` re-exports them.
- **Pool**: `database::PoolCfg::indexer()` / `webserver()` / `relayer()` presets.

## Adding a new binary

1. Match the layered tree above.
2. Implement `shared::tick::TickService` if it's a polling worker.
3. Wire `shared::shutdown` + `shared::tick::run`.
4. Use `database::PostgresCursorRepo` for cursor storage.
5. Use `webserver_shared::AppError` if it's an HTTP server.
