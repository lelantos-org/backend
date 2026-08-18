# fmd-webserver

Read-only HTTP API for FMD clients: register a detection key, pull matches and
note payloads, and sync the commitment and nullifier sets locally. Axum +
Postgres, over the tables `fmd-indexer` writes. Depends on `fmd-crypto`.

It writes exactly one table — `subscriptions` — and reads everything else.

## Run

```sh
DATABASE_URL=postgres://… cargo run -p fmd-webserver
```

## Env

| Var | Required | Default | Notes |
|-----|----------|---------|-------|
| `DATABASE_URL` | yes | — | Postgres URL |
| `BIND_ADDR` | no | `0.0.0.0:3001` | Listen address |
| `INDEXER_LAG_WARN_BLOCKS` | no | `50` | Parsed, but **nothing reads it** — see below |

⚠️ `INDEXER_LAG_WARN_BLOCKS` is dead config. It is loaded into
`FmdWebserverConfig` and never used: `/health` is a static `"ok"` with no DB
round-trip and no lag check. Setting it does nothing.

## Routes

| Route | Auth | Cache-Control |
|-------|------|---------------|
| `GET /health` | — | `no-store` |
| `GET /v1/notes?chainId=&after=&limit=` | — | `public, max-age=3` |
| `GET /v1/matches?chainId=&after=&limit=` | Bearer | `no-store` |
| `POST /v1/subscriptions` | — | `no-store` |
| `DELETE /v1/subscriptions` | Bearer | `no-store` |
| `GET /v1/chains/:chain_id/commitments/chunks/:chunk_id` | — | (default) |
| `GET /v1/chains/:chain_id/nullifiers/chunks/:chunk_id` | — | (default) |
| `GET /v1/tree-state?chainId=` | — | `public, max-age=5` |

Query and body fields are camelCase on the wire. `limit` defaults to 100 and is
clamped to `1..=1000`. `chainId` is required on `/v1/matches` and
`/v1/tree-state`, optional on `/v1/notes`.

Cache-control is per route rather than global. Token-keyed responses are
per-caller data that a shared proxy must never hold and a browser has no reason
to; public feeds get a short TTL.

## Capability tokens

`POST /v1/subscriptions` takes `{detectionKeyHex, gamma, tokenHex}` and
registers the key. The client mints its own 32-byte token; only its hash is
stored.

Subsequent calls present it as `Authorization: Bearer <hex>` — a request
*header*, not the URI. Proxies and CDNs write the URI to access logs by default
and browsers retain it in history; neither applies to a header. The tracing
spans skip both the token and the subscription body for the same reason, and
the span for `/v1/matches` records only the paging cursor.

An absent or non-bearer header is `401`; a well-formed header naming an
unregistered token is `404`, raised by the repository lookup. The two stay
distinct so the status never reports whether a given token exists.

## γ and the decoy floor

γ sets the false-positive rate at `2^-γ`, and the protocol range is `1..=16`.
Higher γ means fewer decoys and a sharper index; lower γ means more privacy.

The server enforces a floor: a subscription's match set must be expected to
contain at least 64 false positives, so the maximum γ it will accept depends on
how many notes exist to draw decoys from. At γ=16 with fewer than 65k notes the
expected decoy count drops below one and the match set simply *is* the user's
note set — which is the thing `matches` exists not to be.

## Chunk feeds

Commitments and nullifiers are served as fixed-size pages of `CHUNK_SIZE = 1024`
so a client can sync the tree without ever telling the server which entries it
cares about. `is_complete` distinguishes a full page from the partial tail.

There is deliberately **no per-commitment Merkle path endpoint**. Asking for the
proof of one `cm` tells the server — and every cache and proxy log on the way —
exactly which note the caller is about to spend. Clients build the tree from the
commitment feed and derive paths themselves.

The commitment feed serves one pre-hashed leaf per entry,
`Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`, as `0x`-prefixed hex:

- **Pre-hashed**, because hashing was the only thing any client did with the raw
  `cm` / `cv_dep`. One field element instead of three cuts the largest feed in a
  cold sync roughly threefold and saves each client ~1M pure-JS Poseidon-4 calls
  over a full tree. Clients can no longer derive the leaf themselves, so they are
  expected to verify the root they build against the on-chain root.
- **`0x`-prefixed**, because the SDK's field decoder accepts decimal *or* hex; a
  bare-hex value whose digits all happen to be decimal would silently parse as a
  completely different number.

Nullifier entries are truncated to their low 10 bytes — the client only tests set
membership, and the feed is downloaded whole by every wallet.

## Chain scoping

`subscriptions` has no `chain_id`: `detection_key` is globally `UNIQUE`, so one
subscription spans every chain a deployment serves, and `matches` tags each row
instead. A detection key is chain-independent, so a note from the wrong chain
still trial-decrypts against the caller's `ivk` — the wallet would store it,
inflate its balance, and be unable to spend it, since the `leaf_index` points
into a different tree. Nothing surfaces until a spend fails, which is why
`/v1/matches` filtering by chain is pinned by an integration test.

## OpenAPI

`utoipa` + Swagger UI mounted by `build_router`. Spec at
`/api-docs/openapi.json`, browsable at `/swagger-ui`.
