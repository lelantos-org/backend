# explorer-webserver

Read-only HTTP API for explorer queries over public chain data. Axum +
Postgres, over the tables `explorer-indexer` writes.

**Must not depend on `fmd-crypto`** — the privacy gate is recorded in
`Cargo.toml` and `lib.rs`, but nothing in CI checks it, so a new dependency
edge has to be caught in review.

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

All read-only `GET`. Every response is cached in-process for `CACHE_TTL_S`,
except `/v1/tree-advances` and `/v1/transactions`, which track the head of the
chain and use a fixed 5s TTL.

| Path | Query | Notes |
|------|-------|-------|
| `/health` | — | Version + git SHA. No DB round-trip. |
| `/v1/assets` | `chainId?` | Includes `priceUsd` / `priceAt` per token; `null` when unpriced. |
| `/v1/tree-advances` | `chainId?`, `sinceStartIndex?`, `limit?` | `limit` clamped to `1..=1000`, default 100. `sinceStartIndex` requires `chainId` (400 otherwise) — `start_index` is per-chain. |
| `/v1/tx-counts` | `chainId?`, `bucketSec?`, `sinceTs?` | `bucketSec` must be a positive multiple of 3600 (default 3600). **Counts commitments, not transactions**: the series is `SUM(tree_advances.inserted)`, so one transaction inserting four leaves counts as four. `/v1/tx-kinds` counts transactions — the two are in different units and do not reconcile. |
| `/v1/chain-flows-24h` | — | 24 whole-hour buckets, oldest first; index 23 is the hour containing the request. One entry per **indexed** chain (`chain_state`): `txCount: 0` means scanned and quiet, absent means not indexed. |
| `/v1/asset-flows` | `chainId?`, `assetIdU64?`, `bucketSec?`, `sinceTs?` | `in`/`out` are **whole tokens** (base units ÷ `10^decimals`, never ÷ `scale`) as plain decimal strings, and `null` unless exactly one asset is in scope — amounts of different tokens are not addable in any unit. `inUsd`/`outUsd` convert each asset at its own decimals and price first, at **current spot**, and cover **only priced assets** — `unpricedAssets` counts the rest. |
| `/v1/transactions` | `chainId?`, `sinceTs?`, `kind?`, `limit?` | Classified feed, newest first. `limit` clamped to `1..=1000`, default 100. An unknown `kind` is a 400 rather than an ignored filter — a caller cannot tell the full feed from a filter that did not apply. |
| `/v1/tx-kinds` | `chainId?`, `bucketSec?`, `sinceTs?` | The same classification, bucketed and pivoted to one row per bucket so the four series stay aligned for a stacked chart. A kind absent from a bucket is a real zero. |
| `/v1/locked` | `chainId?` | Escrowed balance per chain: all-time deposits minus withdrawals, richest first. `amount` is whole tokens per asset (`null` when decimals are unresolved); `lockedUsd` is the only cross-asset total and covers only priced assets, with `unpricedAssets` counting the rest. A negative amount means the index is missing deposits — reported, not clamped. |
| `/v1/anonymity-set` | `chainId?`, `assetIdU64?`, `limit?`, `recentSec?` | Withdrawal cohorts per denomination, over **all history** — an anonymity set is every withdrawal of that size the pool has seen, so this endpoint takes no `sinceTs`; windowing it would report a smaller `k` than a user actually has. `publicOut` is a decimal **string** (a `uint64` exceeds both `i64` and JSON's exact-integer range) and is an opaque key: it is a fixed circuit integer while the yield index moves what it is worth. Rows with no recorded `publicOut` are **skipped, not counted as zero**. Runs index-only off `asset_flows_public_out_covering_idx`. `limit` clamped to `1..=1000`, default 100. `recentSec` (default 30d, clamped) adds a second `recentCount` per row over that lookback — a **subset** of `count`, never a filter on it, so a dormant denomination keeps its full historical `k` and reports `recentCount: 0`. The window is floored to the hour before it reaches the cache key. |
| `/v1/pool-notes` | `chainId?` | Commitment-tree occupancy per chain. `leaves` is the contract's `committedCount` and includes spent notes and relayer fee notes; `feeNotes` counts flushed deposits, since each deposit occupies `LEAVES_PER_DEPOSIT = 2` adjacent leaves and the second pays whoever flushed it, so `leaves - feeNotes` belongs to users. **Never sum across chains** — each chain has its own tree, so notes on one are no cover on another. |

## OpenAPI

`utoipa` + Swagger UI mounted by `build_router`. Spec at `/api-docs/openapi.json`, browsable at `/swagger-ui`.

## Transaction classification

`/v1/transactions` and `/v1/tx-kinds` share one SQL statement, so the feed and
the counts can never disagree about a kind. The split is exact rather than
heuristic, because the contract emits from a bounded set of sites:

| tx | `AssetMoved` | `RootAdvanced` | kind |
|----|--------------|----------------|------|
| deposit / depositAuthorized | `(in>0, 0)` | no | `pending` |
| …once flushed | | (the flush) | `deposit` |
| withdraw | `(0, out>0)` | yes | `withdraw` |
| transfer | none | yes | `transfer` |

Both sides of an `AssetMoved` can never be non-zero — `withdraw` reverts on
`publicIn != 0` and every spend entry point forces `publicIn == 0` — so the sign
of an `asset_flows` row *is* the label.

A deposit counts at flush time, because that is when its note enters the tree;
until then it is `pending` at its escrow time. So a bucket's composition can
still change after the fact. `DepositFlushed` is emitted per deposit inside
`flushBatch`, so a batch of eight counts as eight deposits, not one.

A row whose kind the SQL and the Rust enum disagree about is dropped with a
warning rather than mislabelled.
