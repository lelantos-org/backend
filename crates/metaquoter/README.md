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
masp_fee_bps  = 0                # optional
```

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `listen_addr` | yes | — | Listen address |
| `race_deadline_ms` | no | 1500 | Per-quoter deadline. A quoter slower than this is dropped from the race, not an error |
| `chains[].chain_id` | yes | — | EVM chain id |
| `chains[].rpc_url` | yes | — | HTTP RPC endpoint |
| `chains[].univ3_quoter` | yes | — | UniV3 `QuoterV2` address, called with `eth_call` |
| `chains[].univ3_adapter` | yes | — | Deployed `UniV3Adapter`; returned as `adapter` so the SDK knows which `ISwapAdapter` the route binds to |
| `chains[].masp_fee_bps` | no | 0 | MASP wrapper fee on the output, in bps |

Per-chain env overlay, same convention as the other binaries:
`METAQUOTER_CHAIN_<id>_{RPC_URL,UNIV3_QUOTER,UNIV3_ADAPTER,MASP_FEE_BPS}`.

⚠️ The overlay only rewrites chains **already declared** in the TOML. A variable
naming a chain with no `[[chains]]` block is silently discarded.

## Routes

| Route | Notes |
|-------|-------|
| `GET /health` | Static `ok`. No RPC round-trip |
| `POST /v1/quotes` | `{chain_id, token_in, token_out, amount_in, slippage_bps}` → best `Quote` |
| `GET /swagger-ui` | utoipa spec at `/api-docs/openapi.json` |

Field names are snake_case on the wire, matching the Rust structs — there is no
camelCase rename here, unlike the explorer API.

`amount_in`, `expected_out`, `min_out`, and `masp_fee` are **decimal strings**,
not JSON numbers: amounts above 2^53 do not survive a JSON number, and the
relayer SDK encodes shielded amounts the same way.

| Failure | Status |
|---------|--------|
| `slippage_bps` > 5000, or `token_in == token_out` | 400 |
| no quoter serves that chain | 404 |
| every fee tier reverted (no pool) | 422 |
| RPC failure, or every venue failed | 502 |

The 5000 bps slippage cap is a typo guard: a value that high almost certainly
means a percentage was passed where basis points were expected.

## Racing

`RacingQuoteService` filters to the quoters that support the chain, runs them
concurrently under `race_deadline_ms` each, and takes the maximum
`expected_out`. A quoter that errors or times out is logged and dropped — one
slow venue must not fail a request another venue could answer. If every quoter
drops out, the answer is `all venues failed` rather than a stale or partial
quote.

Today there is exactly one quoter (Uniswap V3), so the race is a race of one;
the shape is what lets a second venue be added without touching the handler.

## UniV3 quoter

Each request fans out over all four canonical fee tiers — 100, 500, 3000,
10000 — with one `eth_call` to `quoteExactInputSingle` per tier, and keeps the
highest output. Tiers with no deployed pool revert at the quoter and are
silently dropped; every tier failing is `NoLiquidity`.

The route blob is `abi.encode(uint24 fee, uint160 sqrtPriceLimitX96)` with the
price limit zeroed. Disabling the pool's own slippage guard is deliberate:
`min_out` is what protects against a sandwich at this layer, and a pool-level
limit would instead revert the whole shielded transaction.

`gas_estimate` is the venue's own estimate plus a fixed 585k of wrapper
overhead — two MASP transacts at roughly 250k each, plus about 85k of wrapper
bookkeeping.

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
