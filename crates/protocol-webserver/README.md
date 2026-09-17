# protocol-webserver

The deployment registry and asset catalog: what chains this deployment serves, what is deployed on them, which assets are registered, what they are worth, and what a yield venue has been paying.

Port **3005**. Read-only over HTTP; the one thing it writes is the rate estimate, and only from an elected replica.

## Why it exists

`GET /chains` on the relayer used to mix two different kinds of fact:

| Fact | Whose? |
|---|---|
| chain name, browser RPC, explorer URL, Permit2, NativeAdapter, SwapWrapper | the **deployment** — identical for every relayer on that chain |
| signer address, mirror root, fee policy | that **one relayer** |

A person self-hosting a relayer to broadcast their own transactions has no business being the authority on Arbitrum's explorer URL, yet the old config made them declare one. Splitting the two means a relayer is configured only with what it operates, and this service — stateless, cacheable, freely replicated — carries the rest.

It also starts undoing a duplication: the relayer and explorer-webserver each had their own `AssetRow` over the same tables, with different field names. The relayer's is gone — it reads `asset-registry` now, as does this service, which is the single route that publishes the catalog. explorer-webserver has yet to follow.

## Routes

| Method | Path | Cache-Control |
|---|---|---|
| GET | `/health` | `no-store` |
| GET | `/v1/chains` | `public, max-age=60` |
| GET | `/v1/assets?chainId=` | `public, max-age={cache_ttl_s}` |
| GET | `/v1/prices` | `public, max-age=60` |
| GET | `/v1/yield-index?chainId=` | `public, max-age=900` |
| GET | `/v1/governance/proposals?chainId=&cursor=&limit=` | `public, max-age=10` |
| GET | `/v1/governance/proposals/{proposalId}?chainId=` | `public, max-age=10` |
| GET | `/v1/governance/proposals/{proposalId}/votes?chainId=&cursor=&limit=` | `public, max-age=10` |
| GET | `/swagger-ui`, `/api-docs/openapi.json` | — |

Every API route also carries an `ETag`, so a wallet re-polling the catalog with `If-None-Match` gets a `304` rather than the body.

Everything here is identical for every caller and carries nothing per-user, which is what makes it safe at a shared cache — unlike the relayer, whose routes carry submissions.

`/v1/prices` moved here from the relayer for the same reason the catalog did: a spot price is a property of the token, identical for every relayer serving the chain, so a self-hosted relayer has no business stating what WETH is worth and a wallet reading it from one would have a figure it cannot cross-check. It is a separate route from `/v1/assets` rather than a field on it because the catalog moves when the indexer registers an asset while a price is stale within the minute — folding them together would mean refetching the catalog to move a price.

## The cross-check this enables

`/v1/chains` publishes the `maspAddress` and `treeDepth` the **deployment** declares. A relayer's own `GET /chains` publishes the same two as that **one relayer** sees them — `maspAddress` read off the submitter it signs with, and `treeDepth` from its compiled-in `tree::DEPTH` rather than from its config, so it is the depth actually being mirrored — alongside the live readings `currentRootHex` and `committedCount`. A wallet fetches both and drops any chain whose two answers disagree.

Those two fields are the *only* overlap. The relayer's `/chains` carries nothing else describing the deployment: no chain name, browser RPC, explorer, Permit2, adapter addresses or asset list. That is what lets a relayer be configured purely with what it operates.

That is the point of the split: it makes a third-party or self-hosted relayer verifiable, which is not expressible while one service asserts both halves.

## Layering

Standard binary layout (see [ARCHITECTURE.md](../../ARCHITECTURE.md)):

| Layer | What |
|-------|------|
| `app/` | Config (TOML plus env overlay), shared state, the in-process caches, build stamp |
| `adapters/` | `rpc` endpoint with this service's archive-read deadline; `venue` share-price and block reads |
| `domain/` | Response bodies, the pure APY arithmetic (`apy`), error type |
| `repositories/` | `yield_samples` (record, window, history, thin) and `asset_yield` (the stored estimate) |
| `services/` | One module per route body (`assets`, `chains`, `prices`, `yield_index`); `venue_apy/` measures a rate from the recorded index (`recorded`) or, until that history fills, the venue's vault (`vault`) |
| `handlers/http/` | The read path: axum routes, router, OpenAPI |
| `handlers/worker/` | The write path: the per-chain elected venue-APY tick loop |

## The venue-APY worker

The one part that is not stateless. It writes `asset_yield_sample` and issues archive `eth_call`s, so running it on every replica would lay down duplicate samples and multiply archive load by the replica count.

A Postgres advisory lock (`NS_VENUE_APY`) elects one measurer per chain; every other replica serves the estimate from the `asset_yield` row it wrote. That election is `handlers/worker/venue_apy.rs`, deliberately a sibling of `handlers/http/`: the two directories are the read path and the write path, and which is which is meant to be visible from the tree. The lock is re-checked each tick — a lock believed held over a dead connection is the one state that would let two replicas measure at once. Failover costs at most one refresh interval, comfortably inside the window after which a stored estimate stops being published at all.

This is also why the estimate is **stored** rather than cached in the process that computed it: the replica that measures is not the replica that answers.

## Configuration

TOML, at `REGISTRY_CONFIG` (default `registry.toml`), because this is the one webserver carrying a per-chain *list* — an array does not fit an env var. Deployed addresses come from the environment as `REGISTRY_CHAIN_<id>_<FIELD>`, the same convention every other binary uses; the overlay only rewrites chains already declared in the file.

Three process-wide values also take an environment override, applied before the per-chain overlay:

| Var | Overrides | Default |
|---|---|---|
| `DATABASE_URL` | `database_url` | — |
| `REGISTRY_BIND_ADDR` | `bind_addr` | `0.0.0.0:3005` |
| `METRICS_ADDR` | `metrics_addr` | `127.0.0.1:3016` |

`apy_rpc_url` is separate from `rpc_url` on purpose: the first is what this service reads the chain with and needs archive state, the second is published to browsers. Absent, `rpc_url` stands in — it measures where a public endpoint happens to serve state a window back, and measures nothing where it does not. With neither set the chain is not measured at all.

## What it does not do

- **No migrations.** Every table it reads is owned by `protocol-indexer`, bar the one it writes itself (`asset_yield_sample`). A webserver deciding another service's table shape would be the wrong way round.
- **No submissions, no quotes, no signing.** Those are the relayer's, and stay there: a quote is a statement by one relayer about what it charges, enforced by its own admission control.
