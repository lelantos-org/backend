# Backend Architecture

## Crate map

```
Libraries
  shared            primitives: event kinds, shutdown, tick driver, the
                    materialized-view refresh rule, config loader, env readers,
                    tracing init; plus `AppError`, the trace span and the
                    `Cache-Control` layer behind the `webserver` feature, and
                    the cache builder behind `cache`
  chain-types       ABI types, log decode and the U256 <-> NUMERIC pair
                    (no DB, no IO); behind its `rpc` feature it also holds
                    `RpcEndpoint`, the timed and retrying JSON-RPC transport
                    every chain-facing service builds its provider on
  common-crypto        FMD primitives (poseidon, baby-jubjub, filter, tree) plus
                    note recognition (trial decrypt + commitment rebuild)
  groth16           native Groth16 over BN254 against snarkjs artifacts: the
                    vendored zkey parser and QAP reduction, the circom
                    witness-graph evaluator, the `tree_update_batch` prover and
                    a circuit-agnostic verifier. Holds the whole arkworks 0.6
                    stack; used by relayer
  database          diesel schema, migrations, bb8 pool, CursorRepo,
                    advisory locks, reorg retraction
  asset-registry    the `assets` x `asset_yield` join, circuit-unit arithmetic
                    (Scale/Rate) and a cached read; used by relayer,
                    registry-webserver and explorer-webserver
  prices            USD spot prices: `PriceProvider` interface, one module per
                    upstream, and a cache-fronted `PriceService`; shared by
                    registry-webserver and explorer-webserver

Binaries
  ingester           live + backfill log ingester (per chain)
  fmd-indexer        FMD consume + filter
  protocol-indexer   asset catalog, yield state, tree advances, deposit ledger
  explorer-indexer   asset-flow and fee analytics for explorer-ui
  fmd-webserver      FMD HTTP API (notes, matches, subscriptions, tree feeds)
  explorer-webserver explorer HTTP API (assets, flows, tree-advances, txs)
  risk-webserver     read-only address screening API
  registry-webserver deployment registry + asset catalog + spot prices; also
                     measures venue APY, single-writer per chain via an
                     advisory lock
  relayer            tree-advance prover + submitter (the only on-chain writer);
                     optionally collects a shielded fee per submission
  metaquoter         DB-less swap quote aggregator

Tests
  integration-tests  cross-crate end-to-end via testcontainers
  test-support       the shared Postgres harness those tests run on
                     (dev-dependency only)
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
- `protocol-indexer` / `explorer-indexer` / `explorer-webserver` → `common-crypto`. This is the privacy gate; it is a convention, not a CI check.
- `groth16` → any other internal crate. It is a leaf, and deliberately arkworks-only: it is what keeps ark 0.6 out of everything `common-crypto` (ark 0.4) links. Public inputs cross its boundary as big-endian `[u8; 32]` words and proofs as decimal strings, so no ark type is nameable by a caller.

## Conventions

- **Service contract**: every long-running indexer service implements `shared::tick::TickService`. Binary main wires it through `shared::tick::run`. Existing examples: `fmd_indexer::services::ConsumeServiceImpl`, `fmd_indexer::services::FilterServiceImpl`, `explorer_indexer::services::consume::ConsumeServiceImpl`.
- **Tick cadence**: `tick_ms` is the *idle ceiling*, never a fixed period. A tick reports `TickProgress` and the driver sleeps only when there is nothing left to do.
- **Repos**: trait + `Postgres*Repo` impl, owned via `Arc<dyn Repo>`. The shared cursor lives in `database::CursorRepo` — do not re-implement it. Advance it with `upsert_monotonic`; plain `upsert` can move it backwards and is for deliberate rewinds only.
- **Reorgs**: a `raw_events` consumer calls `database::reorg::apply_pending` at the top of its tick, before reading.
- **Shutdown**: every binary uses `shared::shutdown::channel()` + `watch_signals(trigger)`. The relayer, which serves HTTP only, uses axum's `with_graceful_shutdown` instead.
- **Config**: TOML-loaded binaries call `shared::config::load_toml(env_var, default_path)`, then `apply_env_overlay()` for the per-chain `<PREFIX>_CHAIN_<id>_<FIELD>` variables, then validate. The overlay only rewrites chains already declared in the TOML.
- **Env vars**: read through `shared::config_env` — `string`/`parse` for plain variables, `lookup`/`lookup_parse` for per-chain ones. Never `std::env::var` directly: an empty variable must count as unset, and a set-but-malformed one must fail the process rather than silently behave as the default.
- **Cache-Control**: `shared::router::cache_control(value)` per route, `public_max_age(secs)` for a configured TTL. Every route declares a policy; there is no router-wide default to inherit by omission.
- **RPC transport**: every chain-facing service builds its provider on
  `chain_types::rpc::RpcEndpoint` (feature `rpc`), passing its own
  `RpcTimeouts`. Never `ProviderBuilder::new().on_http(url)` — alloy's default
  client has **no request timeout**, so a node that accepts the connection and
  never answers hangs the caller forever. `chain-types/tests/rpc_timeout.rs`
  pins the deadline. `ingester` is the one exception: it owns a domain-shaped
  `ChainRpc` trait and sets both timeouts from config.
- **Webserver errors**: `shared::http::AppError` + `AppResult` are the canonical types. Crate-local `domain/error.rs` re-exports them. `relayer` and `metaquoter` define their own, because both need variants the shared type does not carry.
- **Pool**: `database::PoolCfg::indexer()` / `webserver()` / `relayer()` presets. Each carries a `statement_timeout`, so one slow query cannot hold a connection until the pool is exhausted; see [database](crates/database/README.md#pool-presets).
- **Indexer writes**: a tick decodes its whole window into a plan, then writes it in one batched call per table — never one statement per event. No enclosing transaction: writes are idempotent and the cursor moves only after they all land, so a crash replays the window. `fmd_indexer::services::consume::CommitPlan` and `explorer_indexer::services::consume::plan::CommitPlan`.
- **Materialized views**: refreshed through a gate that rebuilds on catching up, or once per interval while behind — never inline on every tick. A whole-table aggregate run per tick makes catch-up quadratic. The rule lives in `shared::refresh::ViewState`; each indexer owns a `RefreshGate` naming its own views (`explorer_indexer::services::consume::RefreshGate`, `protocol_indexer::services::consume::RefreshGate`).
- **Replicas**: `ingester` and `fmd-indexer` are safe as N replicas via advisory locks — failover, not scale-out. The webservers are stateless and scale freely. `relayer` must run **one process per chain**. `registry-webserver` serves freely
from N replicas, but its venue-APY worker elects one measurer per chain via
`database::advisory` (`NS_VENUE_APY`) and stores the result, so the replica that
measures need not be the one that answers.

## Adding a new binary

1. Match the layered tree above.
2. Implement `shared::tick::TickService` if it's a polling worker.
3. Wire `shared::shutdown` + `shared::tick::run`.
4. Use `database::PostgresCursorRepo` for cursor storage, and call `database::reorg::apply_pending` if you consume `raw_events`.
5. Use `shared::http::AppError` (feature `webserver`) if it's an HTTP server.
6. Add it to the workspace `members`, give it a README, and add it to the map above.
