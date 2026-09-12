# rpc-proxy

A caching, rate-limiting JSON-RPC proxy for EVM **reads**.

It exists so browsers get a reliable RPC endpoint without shipping the paid
upstream key to them. Public endpoints are too unreliable for the wallet's read
path; an Alchemy key cannot be handed to a browser. This sits between.

## What it does, in the order a request meets it

1. **Admission charge** — before the body is parsed. One unit plus one per
   32 KB, against the per-IP buckets only. Deserializing up to 256 KB is the
   most expensive thing an unauthenticated caller can ask of this process, and
   what a request costs is not knowable until it has happened; without a charge
   here a flood of oversized garbage is free.
2. **Rate limit** — GCRA in compute units, keyed on client IP, plus a per-chain
   global bucket. Charged before the cache, because the limiter bounds this
   service's own sockets as well as the upstream bill. Over budget is `429` with
   `Retry-After`; a request heavier than a bucket's whole burst, which no wait
   can admit, is `400` naming its cost and the limit — viem retries 429 and 413
   but not 400.
3. **Allowlist** — nine methods, and for `eth_call` a set of
   `(contract, selector)` pairs. Everything else is `-32601`/`-32602`. A
   Multicall3 `aggregate3` is checked call by call.
4. **Cache** — three TTL classes chosen by finality, with request coalescing.
5. **Upstream** — at most `upstream_max_inflight` calls in flight per chain,
   over a total budget that fits inside the request deadline. `eth_call` misses
   at one block are packed into one Multicall3 call where the chain has it.

The per-IP buckets are **shared across chains**; only the credit guard is per
chain. Size `[rate_limit]` for a client's total traffic — a wallet reading three
chains boots at ~144 units, not 48.

## Reads only

`eth_sendRawTransaction`, `eth_estimateGas`, `eth_getTransactionCount` and the
fee methods are **not** served, and this is deliberate:

- Browser writes never come here. They go through the user's own wallet over
  EIP-1193, which supplies its own RPC.
- A public, unauthenticated `eth_sendRawTransaction` is an open transaction
  relay attached to our provider account.
- `eth_estimateGas` is unbounded arbitrary-calldata compute — the cheapest way
  to burn a credit budget.

Consequence: `connect({ privateKey, rpcUrl: <this proxy> })` will not work.
Server-side callers using `PrivateKeySigner` point at the upstream directly.

## The `eth_call` allowlist is static

The set of contracts comes from config and changes only on redeploy. There is no
runtime dependency on registry-webserver.

**So adding an asset on-chain without an `rpc-proxy` converge breaks it** — its
`balanceOf`/`symbol`/`decimals` return `-32602`, and in the UI the transparent
balance hint silently disappears and the earned column shows `—`. Per-asset and
quiet, which is why there are three mitigations rather than one:

- the add-a-token runbook in `infra/README.md` includes an `rpc-proxy` converge,
- every rejection logs the address at WARN,
- `rpc_proxy_rejected_target_total{class="unknown"}` should alert on non-zero.

`erc20_seed` and `venue_seed` are **generated at template time** from the
registry's `/v1/assets`, not hand-written. `venue_seed` especially: `ERC4626Venue`
addresses are `CREATE`-derived at deploy and appear in no config file.

## Archive state is not required

Historical reads — an `eth_call` at an explicit block — are permitted but are no
longer issued by any browser path. Note cost basis, the one thing that needed
them, is served from the registry's recorded index history instead: a per-chain
body identical for every caller, so it also stops the set of blocks a wallet
asks about identifying that wallet.

`upstream_archive` therefore describes a capability rather than a requirement.
An endpoint that prunes serves every read the webapp makes.

## The deployment floor

`deploy_block` refuses historical **state** reads below the block the contracts
were created in. Every `eth_call` target here is one of our own contracts, so a
read below that block asks a paid archive node about state that cannot exist.

It applies to `eth_call` and `eth_getBalance` at an explicit height, and rejects
an `eth_getLogs` range that *ends* before the floor.

It deliberately does **not** floor a log range's `fromBlock`. The SDK's
`defaultFromBlock` returns genesis whenever the chain tip is under its
3600-block lookback — which is every dev chain and every fresh e2e run — so
flooring it would break deposit cancellation there. The range cap is what bounds
that cost instead.

`eth_getBlockByNumber` is also exempt: a block header before the deployment is a
coherent question that needs no archive state, so refusing it would add risk for
no saving.

Unset disables the floor. In dev it is injected by `deploy-contracts.sh`.

## Caching

| Class | TTL | Budget | Holds |
|---|---|---|---|
| `head` | 1 s | 1 MiB | `eth_blockNumber`, `getBlockByNumber("latest")` |
| `recent` | 2 s | 16 MiB | anything at the tip, or mined within `reorg_depth` |
| `finalized` | 1 h | 64 MiB | anything past `reorg_depth`: reads at or over such heights, and blocks, receipts and transactions mined that deep |

The budget is **bytes, not entries** — ~81 MiB per chain. Values here differ in
size by orders of magnitude (a block number is a dozen bytes, an `eth_getLogs`
result can be hundreds of kilobytes) and the key space is caller-shaped, since
a caller picks the block range and topics. An entry ceiling sized for the small
case is no ceiling at all once the large ones arrive. Watch
`rpc_proxy_cache_entries` to tune it.

It is weighted towards `finalized`: a two-second class only ever holds what
arrived in the last two seconds, while an hour-long one keeps turning capacity
into hits. The split is a starting point, not a measurement.

Properties worth knowing:

- **An unknown chain head is safe.** Everything classifies as if it were at the
  tip, so a cold start can only fail to cache something, never serve a stale
  answer.
- **A `null` receipt, transaction or block-by-hash is never cached.** A receipt
  means "not mined yet", and caching it would add a full TTL of latency to
  every deposit confirmation. A block hash can come back `null` from a
  load-balanced upstream one block behind. Found, each is classified by its
  depth.
- **An `eth_call` revert is cached like a result**, in the same class and for
  the same TTL: it is the chain's answer at that block. Every other upstream
  error (`header not found`, provider limits, `missing trie node`) describes the
  node and is asked again. A call that sets its own `gas` never has its revert
  cached, since `gas` is not part of the key.
- **The head warm-up goes through the cache.** An open-ended `eth_getLogs`
  cannot be range-checked without a head, so one is resolved first. That goes
  through the `head` cache like any other read, so a burst arriving while the
  tip is unknown costs one upstream call rather than one per request — which
  matters because the window is a cold start in the normal case but permanent
  if an upstream keeps answering with a block number the tip tracker cannot
  parse.

Coalescing and batching both apply to every call. A request claims all of its
misses at once, forwards the ones nobody else has in flight as one round trip,
and waits on the rest — so a herd of wallets whose batches each carry the head
poll still fetches the head once. See the module docs on `services::proxy`.

## Multicall3

When a round trip carries two or more `eth_call` misses at the same block, they
go upstream as one `aggregate3` call to Multicall3 at
`0xcA11bde05977b3631167028862bE2a173976CA11`. A provider that meters per call
bills the set once. Clients see nothing of it: they send ordinary calls, and
each result is cached, coalesced and rate-limited under its own key as before.

- **Probed, not configured.** The first packable batch asks `eth_getCode` at
  that address once per chain, and the answer is logged at INFO. A chain without
  it — a bare anvil — forwards calls one by one, as before. A failed probe is
  retried on the next packable batch.
- **Only context-free calls are packed.** Inside a pack `msg.sender` is
  Multicall3, gas is shared and no value is sent, so a call with `from`, `gas`
  or `value` goes out on its own. The allowlist is what makes the rest safe:
  every function it admits is a view over its arguments.
- **Only successes are trusted.** A failed member might be a revert or might be
  the pack running out of gas, and a pack answering `0x` (no code at that
  block, or a dev chain reset under a running proxy) answers nothing. Those
  calls are asked again unpacked in a second round trip, so the verdict a
  caller sees — and that the cache may keep — is always the node's own.

`rpc_proxy_upstream_units_total` counts a pack as one call, since that is what
the provider bills.

### A client's own multicall

A client may send an `aggregate3` itself — viem does with `batch.multicall`.
It is **unbundled**, never forwarded whole: forwarded, its calldata would be
unique to that wallet's mix of reads and never hit the cache, and the calls
inside it would be invisible to the allowlist and the rate limit.

- **Each inner call is served as the `eth_call` it is**: validated against the
  allowlist, keyed, cached, coalesced with other callers, and packed again on
  the way up with whatever else is missing. A plain read and the same read
  inside a multicall share one cache entry, in both directions.
- **Charged per inner call**, and the multicall's own calldata is held to
  `max_call_data_bytes`, which bounds how many calls one can carry.
- **Refused whole** if any inner call is: the error names the call's index, and
  nothing is fetched. An inner call to an unlisted contract logs the same WARN a
  lone call would.
- **Answered as Multicall3 would.** A revert the caller allowed is reported in
  place with the node's revert data; one it did not allow reverts the whole
  call with `Multicall3: call failed`. A node error (not a revert) fails the
  whole call.
- **Only `aggregate3`.** `aggregate`, `tryAggregate` and the rest are refused
  with `-32602`; nothing here issues them.

The outer call's `from`, `gas` and `value` are not carried to the inner calls,
since inside `aggregate3` none of them reaches one.

## Tuning `reorg_depth`

The default 64 is mainnet-flavoured. Arbitrum and Base produce blocks far
faster, so 64 blocks there is seconds of wall clock rather than minutes. Start
conservative (~300 on the L2s) and tune from the `finalized`-class hit ratio.

## Behind Cloudflare

Cloudflare gives this service TLS termination, DDoS absorption and response
compression. It gives it **no caching**: JSON-RPC is POST, and Cloudflare does
not cache POST by default. `Cache-Control: no-store` on every route therefore
costs nothing at the edge — it is there to stop any intermediary storing a
response body, which names a specific wallet's balance or a specific deposit.

Do not add compression here. Cloudflare compresses `application/json` on the way
out, and compressing at the origin would only make it decompress first.

Two settings matter:

- `trusted_client_ip_header = "CF-Connecting-IP"`. Cloudflare overwrites that
  header, so it cannot be spoofed through. `X-Forwarded-For` is *appended* to,
  which is why `trusted_client_ip_position` defaults to `rightmost`.
- **The origin must not be reachable except through Cloudflare.** If it is, the
  client address is whatever the caller claims and the per-IP limiter is
  decorative. The compose deployment publishes on loopback behind Caddy, which
  is what makes the header trustworthy.

Cloudflare's origin timeout (100s) is far above this service's 12s deadline, so
a slow upstream surfaces as our own 503 rather than a 524.

When the provider itself rate-limits us — every endpoint answering `429` — the
client gets `503` with the provider's `Retry-After`, clamped to 1–60s (1s when
the provider gave none, or gave a date), rather than a `502` it would back off
from blind. Every caller waiting on the same in-flight call gets the same.
Provider refusals show as `rpc_proxy_upstream_calls_total{outcome="throttled"}`.

### What the origin does not defend itself against

The controls in this crate all bound the cost of a *request*. Nothing here
bounds the cost of a **connection**: the process runs `axum::serve` on hyper's
defaults, so there is no header-read timeout, no keep-alive idle timeout and no
cap on accepted connections. A slowloris client dribbling headers, or one simply
holding sockets open, is bounded by the file-descriptor limit and by nothing
else — the request deadline does not start until hyper has parsed a complete
request head.

That is a deliberate delegation to the edge, and it is only true while the edge
is there. Fixing it in-process means replacing `axum::serve` with a hand-rolled
`hyper_util::server::conn::auto` accept loop, which is where to start if this is
ever exposed without Cloudflare in front.

## Privacy

This service observes in one place an aggregate that was previously distributed
across public RPCs: which wallet reads which balances, and which deposit ids are
watched.

Accordingly: **request parameters are never logged**, and the rate-limit key is
logged as a truncated hash rather than an IP. An `eth_call` `data` field is a
wallet's read pattern, and `eth_getLogs` topics name a specific deposit. The one
exception is the contract address of a refused `eth_call`, which is what makes
a stale allowlist diagnosable.

## Logs

Every line logged while serving a request sits in an `rpc` span carrying
`client` (the 12-hex-digit digest above) and `calls` (the batch size), inside
the router's `http` span with the path — and so the chain id.

| Level | What |
|---|---|
| INFO | startup: build, where the client address is read from, each chain wired; the Multicall3 probe result, once per chain |
| WARN | a refused contract or function (throttled); an upstream call that failed on every endpoint, with its cause, attempts and elapsed time; a Multicall3 pack that did not unpack (throttled) |
| ERROR | an internal error, with its detail; every 5xx, from the router's trace layer |
| DEBUG | rate-limit refusals, malformed calls, each failed upstream attempt, each upstream round trip's packing |

- **Callers cannot flood the log.** A WARN a caller can trigger per request is
  written once per distinct target per 10 minutes, per chain; the metrics still
  count every occurrence. Rate-limit refusals and malformed calls stay at DEBUG.
- **The upstream URL never appears.** It carries the API key, and reqwest puts
  it in every transport error's text, so it is stripped where the error is
  made. The cause (connection refused, TLS, HTTP status) is kept.

Verbosity is `RUST_LOG`, read by `tracing`'s `EnvFilter` and defaulting to
`info` — e.g. `RUST_LOG=info,rpc_proxy=debug` for this crate's DEBUG lines alone.

## Operating

- `GET /health` — liveness, and the endpoint `webapp-ui`'s health indicator
  should probe so the dot stops showing green during an RPC outage.
- `POST /v1/{chain_id}` — standard JSON-RPC, single or batch.
- `/metrics` on `metrics_addr`, loopback, never published.

**Upstream calls saved** = `rpc_proxy_requests_total` −
`rpc_proxy_upstream_calls_total` is the primary effectiveness metric.

Three gauges report the occupancy of the bounds this service leans on, refreshed
by a sweep every 60s:

| Metric | Watch for |
|---|---|
| `rpc_proxy_ratelimit_keys{bucket}` | Sustained growth. The key is an unauthenticated IP prefix, and the sweep is the only thing that retires one — `governor` never does it on its own. |
| `rpc_proxy_cache_entries{chain,class}` | Tuning input for the byte budgets above. |
| `rpc_proxy_upstream_permits_available{chain}` | A sustained zero means requests are queueing against their own deadline; raise `upstream_max_inflight` or find out why the upstream slowed down. |
