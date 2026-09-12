# shared

Bottom of the stack: the runtime primitives and entity types every binary sits
on. No IO loop of its own, no domain logic, and — deliberately — no internal
dependencies at all, not even `database`. Anything that needs a connection
belongs one layer up.

## Modules

| Module | What it owns |
|--------|--------------|
| `entities` | `EventKind` and `Consumer`: the persisted event discriminants and the one-event-one-owner partition |
| `chain` | `ChainId` newtype over `i64` |
| `tick` | `TickService` trait + `run` driver for polling workers |
| `refresh` | When a materialized view is due for a rebuild (`ViewState`) |
| `backoff` | Exponential backoff used by the tick driver |
| `shutdown` | `channel()` / `watch_signals()` graceful-stop primitive |
| `config` | `load_toml(env_var, default_path)` |
| `config_env` | env reads: `string` / `parse`, plus the `<PREFIX>_CHAIN_<id>_<FIELD>` per-chain overlay |
| `tracing_init` | `RUST_LOG`-driven subscriber, defaults to `info` |
| `build_info` | the `build_info!` macro and its startup banner, expanded in the calling crate |
| `metrics` | the metric-name constants, the closed label sets, and the HTTP tracking layer |
| `cache` *(feature `cache`)* | `moka` cache builder |
| `http` *(feature `webserver`)* | canonical `AppError` / `AppResult` for the HTTP crates |
| `request_span` | *(feature `webserver`)* path-only tracing span |
| `router` *(feature `webserver`)* | per-route `Cache-Control`, conditional GET, and the service layer stack |

`http`, `request_span` and `router` are behind the `webserver` feature so the
indexers do not pull axum in. `cache` has its own switch, which `webserver`
implies: a library wanting a cache should not have to link an HTTP framework to
get one — `asset-registry` is the case that split them.

`entities` holds only what more than one crate reads. The row-shaped structs
that used to live beside `EventKind` (`RawEvent`, `Note`, `Subscription`,
`Asset`, `TreeAdvance`, `ChainState`, `ConsumerCursor`) had no consumer outside
this crate — the per-crate diesel row types superseded them — and are gone.

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

## Reading the environment

| Function | Returns |
|----------|---------|
| `string(key)` | `Option<String>` |
| `parse::<T>(key)` | `Result<Option<T>, ParseError>` |
| `lookup(prefix, chain_id, field)` | `Option<String>` for `<PREFIX>_CHAIN_<id>_<FIELD>` |
| `lookup_parse::<T>(prefix, chain_id, field)` | the same, parsed |

**An empty variable counts as unset**, in all four. Compose and k8s render an
unset substitution as the empty string, so a variable that arrived empty carries
no operator intent; honouring it would overwrite a good TOML value with nothing.

`parse` returns `Err` on a value that is set but malformed, rather than falling
back to the default — a typo'd `CACHE_TTL_S=30s` must not serve the default TTL
while the operator believes it took effect. Binaries with their own error enum
wrap it in a one-line local helper (`ingester::app::config::env_parse`).

### Per-chain overlay

`lookup` reads `<PREFIX>_CHAIN_<chain_id>_<FIELD>`. Each binary applies it in its
own `apply_env_overlay`.

```sh
INGESTER_CHAIN_31337_POOL_ADDRESS=0xabc…
RELAYER_CHAIN_31337_SIGNER_KEY=0x59c6…
```

⚠️ Every binary's overlay only rewrites chains **already declared** in its
TOML. A variable naming a chain with no `[[chains]]` block is silently
discarded.

## Cache-Control

`router::cache_control(value)` is the per-route `Cache-Control` layer every
webserver uses; `cache_control_value` takes a runtime `HeaderValue` and
`public_max_age(secs)` builds one from a configured TTL.

Applied per route rather than once per router: cacheability is a property of the
resource, not the service. The layer is *overriding*, so a route's declared
policy wins over anything a handler set — a handler cannot quietly make a
private response shareable.

## Conditional GET

`router::etag` is middleware: it hashes a buffered `200` body (SHA-256 truncated
to 128 bits), sets a strong `ETag`, and answers `304 Not Modified` when the
client's `If-None-Match` covers that tag. Comparison is the weak one RFC 9110
mandates, so `*` and a `W/`-prefixed tag both match.

Opt in per route or per router. It earns its hash where a body is large and
re-requested while unchanged — an analytic endpoint on a TTL, a catalog a wallet
re-polls. It earns nothing on a `no-store` route, since a client that may not
store the body has nothing to revalidate, nor on an `immutable` one, which is
never re-fetched. Bodies without an exact size hint (a stream) or above 4 MiB
pass through untagged rather than being collected.

## Service layers

`router::service_layers(router, limits)` wraps the routes in the deadline, the
body cap, the trace span and the HTTP metrics, in the one order that works — the
metrics layer outside the trace layer, both below `with_state` so `MatchedPath`
is populated. `Limits::default()` is a 30 s deadline and a 64 KiB body;
`Limits::read_only()` narrows the body to 16 KiB for a service with no route
that takes one. A timed-out request is answered `503`, not `408`: the request was
valid and the service was not.

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
