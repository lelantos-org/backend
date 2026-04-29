---
name: layered-binary
description: Organize Rust binary crates with strict layered architecture. Use when scaffolding a new binary (indexer, webserver, worker, ingester) or restructuring an existing one. Defines layer boundaries, module layout, dependency rules, and wiring conventions.
---

# Layered Binary Architecture

Guidelines for organizing binary crates in this workspace. Each binary = one process, one config, one composition root, strict layer separation.

---

## 1. Layers

Bottom-up. Dependencies point **downward only**. Upper layer imports lower; never reverse.

```
main.rs                  — composition root (wiring)
  └── app/               — bootstrap, config, AppState, shutdown
       └── handlers/     — transport (HTTP routes, CLI cmds, queue consumers)
            └── services/ — domain logic, orchestration
                 └── repositories/ — persistence trait impls
                      └── adapters/ — external IO (DB pool, RPC clients, queues)
                           └── domain/ — pure types, errors, no IO
```

Rule: a layer talks to the one directly below via traits. Skipping layers = smell.

---

## 2. Crate Layout

```
crates/<binary-name>/
├── Cargo.toml
└── src/
    ├── main.rs              # composition root only
    ├── lib.rs               # re-exports for tests/external consumers
    ├── app/
    │   ├── mod.rs
    │   ├── config.rs        # Figment/serde config struct + load()
    │   ├── state.rs         # AppState { Arc<dyn Service>, ... }
    │   └── shutdown.rs      # signal handling, graceful drain
    ├── domain/
    │   ├── mod.rs
    │   ├── models.rs        # plain structs/enums, no IO
    │   └── error.rs         # thiserror domain errors
    ├── services/
    │   ├── mod.rs
    │   └── <feature>.rs     # trait + impl, deps as Arc<dyn _>
    ├── repositories/
    │   ├── mod.rs
    │   └── <entity>.rs      # trait + Postgres/Redis/etc impl
    ├── adapters/
    │   ├── mod.rs
    │   ├── db.rs            # pool builder, DatabaseProvider impl
    │   └── rpc.rs           # external RPC client wrapper
    ├── handlers/
    │   ├── mod.rs
    │   ├── http/            # axum routes, extractors, responders
    │   ├── cli/             # subcommands
    │   └── worker/          # tokio::spawn loops
    └── test/
        ├── mod.rs
        ├── handlers/        # mocked unit tests
        ├── integrations/    # testcontainers
        └── helpers/
```

---

## 3. Layer Rules

### `domain/`
- Pure. No `tokio`, no `diesel`, no `reqwest`. Compiles offline.
- Models, value objects, error enums (`thiserror`).
- Used by every layer above.

### `adapters/`
- Owns external IO primitives (pools, clients, channels).
- Exposes connection-getter traits (e.g. `DatabaseProvider`).
- No business logic. No knowledge of `services/` or above.

### `repositories/`
- One trait per aggregate/entity. `#[cfg_attr(test, mockall::automock)]`.
- Impls generic over adapter traits: `PostgresFooRepo<D: DatabaseProvider>`.
- Returns `domain::` types, never raw rows.

### `services/`
- Domain orchestration. Composes repositories + other services.
- Constructor: `pub fn new(repo: Arc<dyn FooRepo>, ...) -> Self`.
- Trait + impl when other services or handlers depend on it.
- No transport concerns (no `axum::`, no `clap::`).

### `handlers/`
- Translate transport → service call → transport response.
- HTTP: extract → call service → map error → respond.
- Worker: loop, poll, call service, log.
- Thin. If logic creeps in here, push it down into `services/`.

### `app/`
- `config.rs`: typed config, env/file loaders, validation at startup.
- `state.rs`: `AppState` holding `Arc<dyn _>` for every service handlers need.
- `shutdown.rs`: `tokio::signal::ctrl_c` + cancellation tokens.

### `main.rs`
- Parse args/config → init tracing → build adapters → build repos → build services → build `AppState` → spawn workers → serve handlers → await shutdown.
- No logic. Wiring only. Should read top-to-bottom like a recipe.

---

## 4. Dependency Wiring

Composition root pattern. All `Arc<dyn Trait>` constructed in `main.rs`:

```rust
// main.rs
let cfg = app::config::load()?;
let db  = adapters::db::pool(&cfg.database).await?;

let foo_repo: Arc<dyn FooRepo> = Arc::new(PostgresFooRepo::new(db.clone()));
let foo_svc: Arc<dyn FooService> = Arc::new(FooServiceImpl::new(foo_repo));

let state = AppState { foo: foo_svc.clone() };

tokio::spawn(handlers::worker::run(foo_svc.clone(), shutdown.clone()));
handlers::http::serve(state, cfg.http, shutdown).await
```

No service constructs its own dependencies. No global singletons. No `lazy_static` for stateful things.

---

## 5. Errors Across Layers

- `domain::Error` — base domain error (`thiserror`).
- `repositories` map adapter errors → `domain::Error`.
- `services` propagate or enrich `domain::Error`.
- `handlers` map `domain::Error` → transport response (HTTP status, exit code, log+continue).
- Never leak `diesel::Error`, `reqwest::Error`, etc. past `repositories/` or `adapters/`.

---

## 6. Testing per Layer

| Layer        | Test type                           | Location                  |
|--------------|-------------------------------------|---------------------------|
| domain       | pure unit                           | inline `#[cfg(test)]`     |
| adapters     | integration (testcontainers)        | `test/integrations/`      |
| repositories | integration (real DB)               | `test/integrations/`      |
| services     | mocked unit (`mockall` on repos)    | `test/handlers/` or inline|
| handlers     | mocked unit (`mockall` on services) | `test/handlers/`          |
| main/app     | smoke / e2e                         | `tests/` (crate root)     |

See `rust-developers` skill for `mockall` and `testcontainers` specifics.

---

## 7. Smells & Fixes

- **Handler imports `diesel`** → push query into a repository.
- **Service imports `axum`** → move transport concern into handler.
- **Repository returns `serde_json::Value`** → map to a `domain::` type.
- **`main.rs` > 200 lines** → extract subgraphs into `app::wire::*` functions, still called from `main`.
- **Cyclic module deps** → a lower layer is reaching up; invert with a trait in the lower layer, impl in upper.
- **`Arc<Mutex<HashMap<...>>>` in `AppState`** → that's a repository; give it a trait.

---

## 8. Checklist for New Binary

- [ ] Crate added to root `[workspace] members`.
- [ ] Directory layout matches §2.
- [ ] `domain/` compiles without async runtime or IO crates.
- [ ] Every external dep behind a trait in `adapters/` or `repositories/`.
- [ ] `main.rs` is wiring-only, reads as a linear recipe.
- [ ] `AppState` exposes services as `Arc<dyn _>`, never concrete impls.
- [ ] Graceful shutdown wired (signal → cancellation token → workers + server).
- [ ] Config validated at startup; binary fails fast on bad config.
- [ ] At least one integration test booting the full wiring against testcontainers.
