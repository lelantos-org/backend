# metaquoter

Swap quote aggregator for shielded swaps. One `POST /v1/quotes` is raced against
every quoter that supports the requested chain, and the best `expected_out`
wins.

**No database.** It holds an RPC provider per chain and nothing else, so it
neither reads nor writes Postgres and needs no migrations.

## Run

```sh
METAQUOTER_CONFIG=metaquoter.toml cargo run -p metaquoter
```

## Config (`metaquoter.toml`)

```toml
listen_addr      = "0.0.0.0:8081"
race_deadline_ms = 1500          # optional

[[chains]]
chain_id      = 31337
rpc_url       = "http://anvil:8545"
univ3_quoter  = "0x…"            # UniV3 QuoterV2
univ3_adapter = "0x…"            # deployed UniV3Adapter, returned to the SDK
univ4_quoter  = "0x…"            # optional, UniV4 V4Quoter
univ4_adapter = "0x…"            # optional, deployed UniV4Adapter
masp_fee_bps  = 0                # optional
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `listen_addr` | yes | — | Listen address |
| `race_deadline_ms` | no | 1500 | Per-quoter deadline. A quoter slower than this is dropped from the race, not an error |
| `chains[].chain_id` | yes | — | EVM chain id. Declaring one twice is refused at startup |
| `chains[].rpc_url` | yes | — | HTTP RPC endpoint. One provider is built per chain and shared by both venues |
| `chains[].univ3_quoter` | yes | — | UniV3 `QuoterV2` address, called with `eth_call` |
| `chains[].univ3_adapter` | yes | — | Deployed `UniV3Adapter`; returned as `adapter` so the SDK knows which `ISwapAdapter` the route binds to |
| `chains[].univ4_quoter` | no | — | UniV4 `V4Quoter` lens address |
| `chains[].univ4_adapter` | no | — | Deployed `UniV4Adapter` |
| `chains[].masp_fee_bps` | no | 0 | MASP wrapper fee on the output, in bps. Must be below 10000 |

Per-chain env overlay, same convention as the other binaries:
`METAQUOTER_CHAIN_<id>_{RPC_URL,UNIV3_QUOTER,UNIV3_ADAPTER,UNIV4_QUOTER,UNIV4_ADAPTER,MASP_FEE_BPS}`.

A chain joins the UniV4 race only when **both** `univ4_quoter` and
`univ4_adapter` are set; with either missing it stays UniV3-only rather than
erroring. When no chain configures V4 the quoter is not constructed at all.

⚠️ The overlay only rewrites chains **already declared** in the TOML. A variable
naming a chain with no `[[chains]]` block is silently discarded.

A variable that is **set but does not parse** fails startup instead of falling
back to the TOML value. These name the deployed adapter a route binds to, so a
truncated address that quietly kept the previous one would emit quotes pointing
at the wrong `ISwapAdapter` with nothing in the log to say so. An empty variable
still counts as unset — compose renders an unset substitution that way, and it
carries no operator intent.

## Routes

| Route | Notes |
|-------|-------|
| `GET /health` | Static `ok`. No RPC round-trip |
| `POST /v1/quotes` | `{chain_id, token_in, token_out, amount_in, slippage_bps}` → best `Quote` |
| `GET /swagger-ui` | utoipa spec at `/api-docs/openapi.json` |

Every route is `no-store`. A quote body names the pair and the amount a caller is
about to trade, which is the correlation the handler keeps out of this service's
own logs; an intermediary cache holding it would have no such policy.

Field names are snake_case on the wire, matching the Rust structs — there is no
camelCase rename here, unlike the explorer API.

`amount_in`, `expected_out`, `min_out`, and `masp_fee` are **decimal strings**,
not JSON numbers: amounts above 2^53 do not survive a JSON number, and the
relayer SDK encodes shielded amounts the same way.

| Failure | Status |
|---------|--------|
| `slippage_bps` > 5000; `token_in == token_out`; either side the zero address; `amount_in` of 0; `amount_in` above `2^128 - 1` with V4 the only venue | 400 |
| no quoter serves that chain | 404 |
| every venue looked and found no pool | 422 |
| every venue failed, at least one without reaching a verdict | 502 |
| every venue exceeded `race_deadline_ms` | 504 |

The 5000 bps slippage cap is a typo guard: a value that high almost certainly
means a percentage was passed where basis points were expected.

5xx bodies do not echo the underlying error. An alloy transport error names the
RPC endpoint, and an endpoint URL is normally an API key; the class is logged
instead and the detail stays at `debug` in the adapter that produced it.

## Racing

`RacingQuoteService` filters to the quoters that support the chain, runs them
concurrently under `race_deadline_ms` each, and takes the maximum
`expected_out`. A quoter that errors or times out is dropped — one slow venue
must not fail a request another venue could answer. Ties on `expected_out` go to
the lower `gas_estimate`, which is then the whole difference in what the caller
nets.

A losing quoter's error is **kept**, not discarded. It is unused whenever any
venue answered, but when none did it is the only thing separating a pair with no
pool from a node that was down, and those are a 422 the caller should stop
retrying and a 502 they should retry. The reported error is the one that makes
the strongest claim about the request, ranked:

```
Internal  >  Rpc  >  Timeout  >  BadRequest / UnsupportedChain  >  NoLiquidity
```

`NoLiquidity` ranks last because it is the only one that asserts something about
the chain. Reporting it over an `Rpc` would state "this pair has no pool" on the
strength of a venue that never got an answer. Ties resolve to the first, so the
answer follows configured venue order rather than completion order.

Uniswap V3 and Uniswap V4 both implement `Quoter`, and neither the handler nor
`RacingQuoteService` knows anything venue-specific — adding the second venue
touched only the wiring in `app/state.rs` and the `Venue` enum.

## Reading a failed venue call

`adapters::call::best_tier` is shared by both venues, because they differ in the
ABI and in what a tier is, not in how a failed `eth_call` should be read.

A tier whose pool is not deployed **reverts** at the lens, which is the ordinary
case for three of the four tiers in a fan-out, so a revert is dropped. Anything
else — a refused connection, `-32005 limit exceeded`, a gateway 5xx, a response
that will not decode against the ABI — reached no verdict and is **not** dropped:
if no tier answered while at least one failed that way, the result is `Rpc`
rather than `NoLiquidity`. Folding the two let one unreachable node report that a
pair has no pool.

A revert is identified by `execution reverted` in the JSON-RPC error message,
matched on text because no code distinguishes it: geth answers `-32000` and
other nodes `3`, and both also cover errors that are not reverts.

## UniV3 quoter

Each request fans out over all four canonical fee tiers — 100, 500, 3000,
10000 — with one `eth_call` to `quoteExactInputSingle` per tier, and keeps the
highest output.

The route blob is `abi.encode(uint24 fee, uint160 sqrtPriceLimitX96)` with the
price limit zeroed. Disabling the pool's own slippage guard is deliberate:
`min_out` is what protects against a sandwich at this layer, and a pool-level
limit would instead revert the whole shielded transaction.

`gas_estimate` is the venue's own estimate plus a fixed 585k of wrapper
overhead — two MASP transacts at roughly 250k each, plus about 85k of wrapper
bookkeeping.

## UniV4 quoter

Each request fans out over the four canonical `(fee, tickSpacing)` pairs —
`(100,1)`, `(500,10)`, `(3000,60)`, `(10000,200)` — with one `eth_call` to
`quoteExactInputSingle` per pair, keeping the highest output.

`hooks` is pinned to the zero address, so only vanilla pools are quoted. A hook
pool can charge a dynamic fee or run arbitrary logic during the swap, and
`UniV4Adapter` refuses to execute against one, so quoting it would hand back an
unexecutable route.

The route blob is `abi.encode(uint24 fee, int24 tickSpacing)`. Currency ordering
is derived from the token addresses by the adapter rather than encoded, so the
route cannot disagree with `token_in`/`token_out`.

V4 takes the input amount as a `uint128` where V3 takes a `uint256`, so an
`amount_in` above `2^128 - 1` is refused by this venue rather than silently
truncated. On a chain that also runs V3 the request still succeeds on V3's
quote; the 400 surfaces only when V4 was the only venue that could have answered.

Native-ETH V4 pools (`currency0 == address(0)`) are **not** quoted; only the
WETH-side pools are reachable today. The handler rejects a zero-address side up
front for the same reason.

### The MASP fee is a reciprocal, not a subtraction

MASP charges its fee *on top of* the deposited amount (`MASP._computeAmounts`),
so the amount that can be deposited out of a gross venue output is
`gross * 10_000 / (10_000 + bps)`, not `gross * (10_000 - bps) / 10_000`.
`expected_out` is that reciprocal figure and `masp_fee` is the difference;
`min_out` then applies the caller's `slippage_bps` to `expected_out`, so the
fee is inside the slippage floor rather than stacked outside it.

`quoted_at` is stamped in Unix seconds so the client can drive its own
staleness UI — nothing here expires a quote server-side.

## Layering

Standard binary layout (`app` / `adapters` / `domain` / `repositories` /
`services` / `handlers`). `repositories::quoter::Quoter` is the venue trait —
named for the layer position, though it is an RPC-backed source rather than a
database one. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
