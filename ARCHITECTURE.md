# Backend Architecture

## Crate map

```
Libraries
  shared            primitives: entities, shutdown, tick driver, config loader,
                    tracing init; plus `AppError` + cache builder behind the
                    `webserver` feature
  chain-types       ABI types + decode (no DB, no IO)
  fmd-crypto        FMD primitives (poseidon, baby-jubjub, filter, tree) plus
                    note recognition (trial decrypt + commitment rebuild)
  database          diesel schema, migrations, bb8 pool, CursorRepo,
                    advisory locks, reorg retraction

Binaries
  ingester           live + backfill log ingester (per chain)
  fmd-indexer        FMD consume + filter
  explorer-indexer   asset / flow / tree-advance / deposit indexer
  fmd-webserver      FMD HTTP API (notes, matches, subscriptions, tree feeds)
  explorer-webserver explorer HTTP API (assets, flows, tree-advances, txs)
  risk-webserver     read-only address screening API
  relayer            tree-advance prover + submitter (the only on-chain writer);
                     optionally collects a shielded fee per submission
  metaquoter         DB-less swap quote aggregator

Tests
  integration-tests  cross-crate end-to-end via testcontainers
```

There is no `webserver-shared` crate: the HTTP error type lives in
`shared::http`, gated behind the `webserver` feature so the indexers do not link
axum.

Every crate has its own README; start from the [root README](README.md) for how
the services fit together.

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
- Any binary → another binary. `integration-tests` is the sole exception, and only as a dev-dependency.
- `database` → any binary or service crate.
- `shared` → anything internal (it is the bottom of the stack).
- `explorer-indexer` / `explorer-webserver` → `fmd-crypto`. This is the privacy gate; it is a convention, not a CI check.

## Conventions

- **Service contract**: every long-running indexer service implements `shared::tick::TickService`. Binary main wires it through `shared::tick::run`. Existing examples: `fmd_indexer::services::ConsumeServiceImpl`, `fmd_indexer::services::FilterServiceImpl`, `explorer_indexer::services::consume::ConsumeServiceImpl`.
- **Tick cadence**: `tick_ms` is the *idle ceiling*, never a fixed period. A tick reports `TickProgress` and the driver sleeps only when there is nothing left to do.
- **Repos**: trait + `Postgres*Repo` impl, owned via `Arc<dyn Repo>`. The shared cursor lives in `database::CursorRepo` — do not re-implement it. Advance it with `upsert_monotonic`; plain `upsert` can move it backwards and is for deliberate rewinds only.
- **Reorgs**: a `raw_events` consumer calls `database::reorg::apply_pending` at the top of its tick, before reading.
- **Shutdown**: every binary uses `shared::shutdown::channel()` + `watch_signals(trigger)`. The relayer, which serves HTTP only, uses axum's `with_graceful_shutdown` instead.
- **Config**: TOML-loaded binaries call `shared::config::load_toml(env_var, default_path)`, then `apply_env_overlay()` for the per-chain `<PREFIX>_CHAIN_<id>_<FIELD>` variables, then validate. The overlay only rewrites chains already declared in the TOML.
- **Webserver errors**: `shared::http::AppError` + `AppResult` are the canonical types. Crate-local `domain/error.rs` re-exports them. `relayer` and `metaquoter` define their own, because both need variants the shared type does not carry.
- **Pool**: `database::PoolCfg::indexer()` / `webserver()` / `relayer()` presets.
- **Replicas**: `ingester` and `fmd-indexer` are safe as N replicas via advisory locks — failover, not scale-out. The webservers are stateless and scale freely. `relayer` must run **one process per chain**.

## Adding a new binary

1. Match the layered tree above.
2. Implement `shared::tick::TickService` if it's a polling worker.
3. Wire `shared::shutdown` + `shared::tick::run`.
4. Use `database::PostgresCursorRepo` for cursor storage, and call `database::reorg::apply_pending` if you consume `raw_events`.
5. Use `shared::http::AppError` (feature `webserver`) if it's an HTTP server.
6. Add it to the workspace `members`, give it a README, and add it to the map above.
