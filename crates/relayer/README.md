# relayer

The only service that writes on chain. A client POSTs a spend or a swap, the
relayer builds the batch witness against its mirror of the commitment tree,
proves `tree_update_batch` with Groth16, and submits the transaction. A cron
worker does the same for escrowed deposits.

It keeps **no tables of its own**: the tree mirror is rebuilt at startup from
`notes` + `tree_advances`, and the pending-deposit queue is re-read from
`deposit_escrowed_events` on every tick.

## Run

```sh
RELAYER_CONFIG=relayer.toml cargo run -p relayer
```

Needs the `tree_update_batch` circuit artifacts. From `stack/`, `just up`
fetches them automatically (`just fetch-circuits` to run by hand).

Migrations run at startup — idempotent, and there in case compose brings the
relayer up before the ingester. Config is validated *after* the env overlay, so
an env-supplied value is checked too, and every problem is reported at once
rather than one restart per mistake.

Shutdown is graceful on SIGINT/SIGTERM: in-flight requests drain, new
connections are refused. A rolling restart mid-submission would otherwise drop
the caller's connection while their transaction may already be in flight,
leaving them unable to tell a failed spend from a landed one.

## Routes

| Route | Notes |
|-------|-------|
| `GET /health` | version + commit |
| `GET /chains` | Per-chain registry: leaf count, current root, MASP + relayer addresses, `desynced`, the wallet-facing config block, and the registered assets |
| `POST /v1/spend` | `transfer` / `withdraw` / `withdrawNative`. Honours `Idempotency-Key` |
| `POST /v1/spend/estimate` | Fee quote for the same payload. Does **not** prove or submit |
| `POST /v1/swap` | Leg-1 SNARK + leg-2 escrow blob via `SwapWrapper` |
| `POST /v1/swap/estimate` | Fee quote for a swap |
| `GET /v1/deposits/stream?chain_id=` | SSE of deposit lifecycle events |

Bodies are capped at 256 kB — generous for a transact payload plus its
per-output ciphertexts, and far below the 2 MB axum would otherwise buffer per
request.

`/chains` is the registry a wallet boots from. The relayer is the only service
that already enumerates every chain, so publishing the wallet-facing half of
its config here is what lets a deployment add a chain without rebuilding any
frontend. It reads the *published snapshot* of the tree, never the mirror
itself: the mirror mutex is held from reserve through confirmation, so locking
it here would park the endpoint every wallet boots from behind whatever
submission is in flight.

`/v1/deposits/stream` rejects a chain the relayer does not serve. A valid stream
that can never emit anything reads to a client as "no deposits yet".

## Submission path

```
parse nullifiers + fingerprint     400 on a malformed payload, before anything is cached
  └─ idempotency (per chain, per key)
       └─ nullifier guard          409 on a double-spend
            └─ verify transact proof   locally, before the tree lock
                 └─ tree mirror lock
                      ├─ reserve leaves
                      ├─ tree_update_batch Groth16
                      ├─ submit + await receipt
                      └─ commit, or rollback the speculative inserts
```

The ordering is the point. Parsing precedes the idempotency run so a malformed
payload is a 400 rather than something cached against a key; the nullifier
reservation happens *inside* it, because a resubmission under a known key must
replay the first answer rather than be refused as a double-spend.

### Nullifier guard

Three layers, cheapest first, all before the tree lock so a doomed submission
never costs a Groth16 or a reverted transaction's gas:

1. Per-chain set of nullifiers this relayer is currently processing — catches
   concurrent duplicates.
2. Per-chain TTL cache (15 min) of nullifiers this relayer has already landed.
   `spent_nullifiers` is written by the indexer, which trails the chain; without
   this layer a resubmit inside that window passes the other two checks.
3. `spent_nullifiers` lookup — catches replays this process did not submit, and
   everything older than the TTL.

Every layer covers all `TRANSACT_IN` nullifiers, not just the first two.

### Idempotency

`Idempotency-Key` is honoured per chain, for 15 minutes — the same window as the
recently-spent cache, past which a resubmit is caught by the spent set instead.
A repeat of an answered key replays the original transaction hash; the same key
with a *different* payload is refused, since the key is pinned to a fingerprint
of what produced the answer. A retry that races the original waits for it, and a
failed submission leaves the key free.

Single-process state, deliberately — as is the nullifier guard, and for the same
reason: the tree mirror is per-process, so two relayers cannot serve one chain
anyway.

### Local proof verification

The wallet's `transact_3x3` proof is verified in-process before the mirror lock
is taken. Without it, the first thing to check a wallet's proof would be the
contract — after a multi-second `tree_update_batch` Groth16 behind a
single-permit gate, holding the chain's tree mutex. Any unauthenticated caller
could therefore spend the relayer's prover on payloads that were never going to
land. Verification is a few pairings: milliseconds against seconds.

This needs `prover.transact_vkey_path`. It is optional so a deployment that has
not shipped the artifact still boots, but it should always be set.

### Tree mirror

One `Arc<Mutex<TreeMirror>>` per chain, depth 10, quaternary, matching
`MASP.MAX_LEAVES`. The pipeline holds the mutex across
`reserve → prove → submit → receipt`, which is what serialises submissions
within a chain: two of them cannot interleave and reorder on chain, and nonces
stay sequential without any explicit nonce management.

On a revert the speculative inserts are unwound. A mirror that cannot be
reconciled parks itself as **desynced**, which `/chains` reports per chain.

### Prover

`ArkCircomProver` parses the `.zkey` once at startup and runs Groth16 over
ark-bn254 behind a **single permit** — proving is CPU-bound, and letting two run
concurrently makes both slower rather than either faster.

## Flush worker

One cron task per chain, every `flush_interval_s`. It re-reads
`deposit_escrowed_events` for deposits that are neither flushed nor canceled,
oldest first by `submitted_at_block`, batches up to `flush_max_n` (clamped to
the contract's `MAX_L_BATCH = 8`, one leaf per deposit), and submits one
`flushBatch`. Each success publishes a `DepositEvent::Flushed` on the broadcast
channel behind `/v1/deposits/stream`.

The channel holds 256 events and drops oldest for a lagging receiver, which is
ample at ≤ 8 deposits per tick.

Because the ledger is re-read every tick, the worker is stateless: a restart
resumes from whatever the indexer has recorded, not from memory.

## Fee estimation

`/v1/*/estimate` validates the payload shape and then quotes — it does **not**
build calldata, prove, or take the tree lock.

Gas *units* come from `gas_witness`, which learns them from the relayer's own
receipts: quoting used to mean building real calldata, which meant a full
`tree_update_batch` Groth16, purely so `eth_estimateGas` had something the
verifier would accept. That put a multi-second single-threaded proof on an
unauthenticated request path, ahead of every real submission waiting on the same
prover.

Each entry point quotes the **high-water mark** of a bounded window of recent
observations rather than the last one. A single observation is a fine predictor
for transfers and withdraws, which are dominated by fixed costs, but swap gas
scales with route length and adapter — a single-hop observation would under-quote
the multi-hop that follows. The window still decays once it has rolled past the
expensive shape.

This is a *fee* quote, not a gas limit: submissions take their limit from alloy's
own per-tx estimate, so a stale value here shifts a little cost between relayer
and user and nothing more.

Gas *price* comes from `gas_estimator`: EIP-1559 when the latest block exposes
`baseFeePerGas`, legacy `eth_gasPrice` otherwise (BSC, some sidechains).

⚠️ On optimistic rollups (Arbitrum, Optimism) execution gas excludes the L1
data-availability fee, which can dominate. This is a known undercount.

Prices come from the `PriceOracle` — Coinbase by default — with a TTL cache,
single-flight per pair, stale-cache fallback within `max_stale_s` on a fetch
failure, and an optional USD cross when the direct pair 404s. Each accepted fee
token is priced concurrently; one that cannot be priced drops out of the quote
rather than failing it. Amount math is scaled integers (1e8), not `f64`, so a
low-decimal token quoted against 18-decimal native does not lose precision.

## Config (`relayer.toml`)

```toml
database_url = "postgres://…"
listen_addr  = "0.0.0.0:3003"

[prover]
wasm_path = "/circuits/tree_update_js/tree_update.wasm"
r1cs_path = "/circuits/tree_update.r1cs"
zkey_path = "/circuits/tree_update_final.zkey"
transact_vkey_path = "/circuits/3x3_verification_key.json"   # optional, but set it

[price_oracle]                    # optional block, all defaults shown
base_url        = "https://api.coinbase.com/v2"
endpoint        = "spot"          # "spot" | "buy"
cache_ttl_s     = 300
max_stale_s     = 300
allow_usd_cross = true

[[chains]]
chain_id       = 31337
rpc_url        = "http://anvil:8545"
pool_address   = "0x…"
signer_key_hex = "0x…"
```

### Chain keys

| Key | Required | Default | Notes |
|-----|----------|---------|-------|
| `chain_id` | yes | — | Must be unique. A duplicate builds two independent tree mirrors and two flush workers for one chain — a guaranteed desync |
| `rpc_url` | yes | — | The relayer's own endpoint, typically cluster-internal. Not what wallets get; see `public.rpc_url` |
| `pool_address` | yes | — | MASP pool, target of `transact` |
| `signer_key_hex` | yes | — | 32-byte hex. Must match the on-chain bound `relayer` address wallets pin in their transact proofs |
| `receipt_timeout_s` | no | 60 | Receipt poll budget. A revert rolls the mirror back and answers 502 |
| `receipt_poll_interval_ms` | no | 250 | Pick ~¼ of block time |
| `flush_interval_s` | no | 30 | Must be > 0 |
| `flush_max_n` | no | 8 | Clamped to `MAX_L_BATCH = 8` |
| `native_adapter_address` | no | — | Enables `withdrawNative`. The SNARK must name it as both `recipient` and `relayer` — the adapter is the pool's caller there |
| `swap_wrapper_address` | no | — | Enables `/v1/swap` |
| `native_symbol` | no | `ETH` | Oracle base for the native gas token |
| `native_decimals` | no | 18 | Must be ≤ 38 |
| `fee_markup_bps` | no | 1000 | 10%. Must be ≤ 1_000_000 |
| `swap_default_deadline_s` | no | 300 | Applied when the wallet pinned no deadline. Without a bound a swap can sit in the mempool and execute at an arbitrarily later price with only `min_out` protecting the user |
| `accepted_fee_tokens` | no | `[]` | `{symbol, address, decimals, quote_symbol}`; decimals ≤ 38 |
| `public` | no | — | Wallet-facing block, served verbatim by `/chains`: `name`, `rpc_url`, `tree_depth`, `permit2_address`, `explorer_url` |

Per-chain env overlay:
`RELAYER_CHAIN_<id>_{POOL_ADDRESS,RPC_URL,SIGNER_KEY,SWAP_WRAPPER_ADDRESS,NATIVE_ADAPTER_ADDRESS,NATIVE_SYMBOL,FEE_MARKUP_BPS}`.

⚠️ The overlay only rewrites chains **already declared** in the TOML. A variable
naming a chain with no `[[chains]]` block is silently discarded.

`stack/config/prod/relayer.toml` ships a zero `signer_key_hex`, which fails
secp256k1 validation at startup — the prod profile needs
`RELAYER_CHAIN_<id>_SIGNER_KEY` set to boot.

## Replicas

**Do not run more than one replica per chain.** Unlike the indexers there is no
advisory lock here: the tree mirror, the nullifier guard, and the idempotency
cache are all per-process state, and two relayers on one chain would reserve
overlapping leaf ranges and race each other's `flushBatch`.

## Layering

Standard binary layout, plus `services/pipeline/` (spend, swap, flush over a
shared `common`) and `services/tree/`. `build.rs` stamps the version and git SHA
that `/health` reports. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
