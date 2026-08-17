# explorer-webserver

HTTP API for explorer queries (public chain data). Axum + Postgres. Must not depend on `fmd-crypto` (CI gate enforces).

## Run

```bash
DATABASE_URL=postgres://... cargo run -p explorer-webserver
```

## Env

| Var | Required | Default | Notes |
|-----|----------|---------|-------|
| `DATABASE_URL` | yes | — | Postgres URL |
| `EXPLORER_BIND_ADDR` | no | `0.0.0.0:3002` | Listen address |
| `CACHE_TTL_S` | no | `30` | Response cache TTL (seconds) |
| `PRICE_BASE_URL` | no | `https://coins.llama.fi` | DefiLlama-compatible price API root |
| `PRICE_TTL_S` | no | `300` | Spot-price cache TTL (seconds) |
| `PRICE_TIMEOUT_MS` | no | `5000` | Upstream price request deadline |

## Prices

Token USD prices come from DefiLlama, batched: one request covers every token
the price cache is missing, keyed by `(chain, address)` — which is exactly what
`assets` stores, so no symbol or decimals index of our own is needed. The
provider returns `decimals` alongside the price, and that is what makes
base-units → USD conversion possible.

Prices are decoration on public chain data, so nothing here can fail a request:

- A chain with no DefiLlama slug (local anvil, `31337`) is **never sent
  upstream** — the dev stack makes no outbound calls at all.
- A token the provider does not know comes back absent; that negative answer is
  cached too, so an unpriceable token is asked about once per TTL, not once per
  request.
- A failed fetch logs a warning, leaves USD fields absent, and is **not**
  cached, so a transient outage retries on the next request.
- Quotes below DefiLlama's own confidence floor are dropped rather than
  reported as dollars.

`priceUsd: null` always means *unknown*, never `0`.

## Endpoints

All read-only `GET`. Every response is cached in-process for `CACHE_TTL_S`, except `/v1/tree-advances`, which tracks the head of the chain and uses a fixed 5s TTL.

| Path | Query | Notes |
|------|-------|-------|
| `/health` | — | Version + git SHA. No DB round-trip. |
| `/v1/assets` | `chainId?` | Includes `priceUsd` / `priceAt` per token; `null` when unpriced. |
| `/v1/tree-advances` | `chainId?`, `sinceStartIndex?`, `limit?` | `limit` clamped to `1..=1000`, default 100. `sinceStartIndex` requires `chainId` (400 otherwise) — `start_index` is per-chain. |
| `/v1/tx-counts` | `chainId?`, `bucketSec?`, `sinceTs?` | `bucketSec` must be a positive multiple of 3600 (default 3600). |
| `/v1/chain-flows-24h` | — | 24 whole-hour buckets, oldest first; index 23 is the hour containing the request. One entry per **indexed** chain (`chain_state`): `txCount: 0` means scanned and quiet, absent means not indexed. |
| `/v1/asset-flows` | `chainId?`, `assetIdU64?`, `bucketSec?`, `sinceTs?` | `in`/`out` are **whole tokens** (base units ÷ `10^decimals`, never ÷ `scale`) as plain decimal strings, and `null` unless exactly one asset is in scope — amounts of different tokens are not addable in any unit. `inUsd`/`outUsd` convert each asset at its own decimals and price first, at **current spot**, and cover **only priced assets** — `unpricedAssets` counts the rest. |
| `/v1/locked` | `chainId?` | Escrowed balance per chain: all-time deposits minus withdrawals, richest first. `amount` is whole tokens per asset (`null` when decimals are unresolved); `lockedUsd` is the only cross-asset total and covers only priced assets, with `unpricedAssets` counting the rest. A negative amount means the index is missing deposits — reported, not clamped. |

## OpenAPI

`utoipa` + Swagger UI mounted by `build_router`. Spec at `/api-docs/openapi.json`, browsable at `/swagger-ui`.
