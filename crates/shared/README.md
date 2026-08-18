# shared

Bottom of the stack: the runtime primitives and entity types every binary sits
on. No IO loop of its own, no domain logic, and — deliberately — no internal
dependencies at all, not even `database`. Anything that needs a connection
belongs one layer up.

## Modules

| Module | What it owns |
|--------|--------------|
| `entities` | Row-shaped types crossing crate boundaries: `EventKind`, `RawEvent`, `Note`, `Match`, `Subscription`, `Asset`, `TreeAdvance`, `ChainState`, `ConsumerCursor` |
| `chain` | `ChainId` newtype over `i64` |
| `tick` | `TickService` trait + `run` driver for polling workers |
| `backoff` | Exponential backoff used by the tick driver |
| `shutdown` | `channel()` / `watch_signals()` graceful-stop primitive |
| `config` | `load_toml(env_var, default_path)` |
| `config_env` | `<PREFIX>_CHAIN_<id>_<FIELD>` per-chain env overlay |
| `tracing_init` | `RUST_LOG`-driven subscriber, defaults to `info` |
| `cache` *(feature `webserver`)* | `moka` cache builder |
| `http` *(feature `webserver`)* | canonical `AppError` / `AppResult` for the HTTP crates |

`cache` and `http` are behind the `webserver` feature so the indexers do not
pull axum and moka in.

## The tick driver

`tick::run` is not a fixed-cadence timer. A tick reports what it accomplished
and the driver sleeps only when there is nothing left to do — otherwise initial
sync would be pinned at `batch / tick_ms` regardless of how fast Postgres could
go.

| `TickProgress` | Meaning | Driver behaviour |
|----------------|---------|------------------|
| `Saturated` | batch came back full | no sleep; go straight round |
| `Partial` | advanced, queue drained | sleep from the 50 ms floor |
| `Idle` | cursor did not move | sleep, doubling up to the configured ceiling |

A round covering several chains takes the **maximum**, so one chain still
holding queued work keeps the whole driver off the sleep path. The configured
`tick_ms` is therefore an idle *ceiling*, never a period.

```rust
#[async_trait]
impl TickService for MyService {
    fn name(&self) -> &'static str { "my-service" }
    async fn list_chain_ids(&self) -> Vec<i64> { .. }
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<TickProgress> { .. }
}
```

A tick that returns `Err` is logged and contributes nothing to the round: one
chain failing must not stall the others, masquerade as progress, or mask
another chain's catch-up.

Implementors today: `fmd_indexer::services::{ConsumeServiceImpl, FilterServiceImpl}`,
`explorer_indexer::services::consume::ConsumeServiceImpl`.

## Backoff

`Backoff::new(initial, max, factor)` panics — with `assert!`, not
`debug_assert!` — on a zero `initial`, an `initial` above `max`, or a factor
below 2. Each of those degenerates into a delay that never grows, which in a
release build is a loop spinning at full speed: precisely the failure the type
exists to prevent.

## Config env overlay

`config_env::lookup(prefix, chain_id, field)` reads
`<PREFIX>_CHAIN_<chain_id>_<FIELD>` and returns `Some` only when the variable is
set and non-empty. Each binary applies it in its own `apply_env_overlay`.

```sh
INGESTER_CHAIN_31337_POOL_ADDRESS=0xabc…
RELAYER_CHAIN_31337_SIGNER_KEY=0x59c6…
```

⚠️ Every binary's overlay only rewrites chains **already declared** in its
TOML. A variable naming a chain with no `[[chains]]` block is silently
discarded.

## AppError

`http::AppError` maps to status codes once, for every webserver:

| Variant | Status |
|---------|--------|
| `NotFound` | 404 |
| `BadRequest` | 400 |
| `Conflict` | 409 |
| `Unauthorized` | 401 |
| `Db`, `Internal` | 500 |

Every 5xx body is the fixed string `internal server error` — the underlying
error is logged, never returned, so a driver message cannot leak schema or
connection details to a caller.

## Layering

May import: nothing internal. Must NOT import `database`, any binary, or any
service crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
