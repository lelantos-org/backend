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
| `GET /chains` | Per-chain registry: leaf count, current root, MASP + relayer addresses, `desynced`, the wallet-facing config block, the registered assets, and the [shielded fee](#shielded-fees) terms where one is charged |
| `POST /v1/spend` | `transfer` / `withdraw` / `withdrawNative`. Honours `Idempotency-Key`. **402** when a required shielded fee is missing or short |
| `POST /v1/spend/estimate` | Fee quote for the same payload, including the note value to pay. Does **not** prove or submit |
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

Every RPC call carries a deadline and a retry budget (`adapters/rpc.rs`), and
routes other than the SSE stream carry one too. That is not belt-and-braces:
the mirror mutex is held from reserve through confirmation, so an untimed call
against a hung node holds the whole chain — and everything queued behind it —
for as long as the node stays hung.

Filling a transaction and broadcasting it are separate steps, because their
failures mean different things. A filler round trip fails before anything is
signed, so nothing landed and the mirror may roll back. An *unanswered*
broadcast may already be in the mempool, so the hash — known before the send —
is resolved against the chain instead of assumed. Only a node that explicitly
refuses is treated as "nothing landed".

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

`Groth16Prover` parses the `.zkey` once at startup and runs Groth16 over
ark-bn254 behind a **single permit** — proving is CPU-bound, and letting two run
concurrently makes both slower rather than either faster.

## Flush worker

One cron task per chain, every `flush_interval_s`. It re-reads
`deposit_escrowed_events` for deposits that are neither flushed nor canceled,
oldest first by `submitted_at_block`, batches up to `flush_max_n` (clamped to
`MAX_DEPOSITS_PER_BATCH = 4` — the contract's `MAX_L_BATCH = 8` at two leaves
per deposit), and submits one
`flushBatch`. Each success publishes a `DepositEvent::Flushed` on the broadcast
channel behind `/v1/deposits/stream` — 256 slots, dropping oldest for a lagging
receiver, which is ample at ≤ 4 deposits per tick.

```
read pending deposits          oldest first, scanning past quarantined
                               and deferred ids
  └─ pre-flight                escrowed(id) + local digest, before the prover
       └─ tree mirror lock
            ├─ reserve leaves
            ├─ tree_update_batch Groth16
            ├─ submit + await receipt
            └─ commit, or roll back and charge the batch
```

Two properties shape the sections below. `flushBatch` is **all-or-nothing**:
one deposit `_drainDeposit` refuses reverts the batch. And the head of the queue
is always in the batch, because the ordering is fixed. So a single deposit that
can never land blocks every newer deposit on its chain — and, without a bound,
does so forever, rebuilding and reproving the same doomed batch every tick.

The same head-of-queue property applies to deposits the worker merely *declines*
to flush, such as one whose fee leaf does not pay this relayer. Those are not
faults and are never quarantined, so the worker instead defers them and the
mempool query scans past what is excluded rather than stopping at the first
`flush_max_n` rows. Both are covered under [Failure budget](#failure-budget).

### Pre-flight

The contract keeps only `escrowed[id]`, a keccak digest over every field the
relayer replays at flush time. Reading that slot back and re-deriving the digest
locally (`domain::deposit_digest`, byte-identical to `MASP._depositDigest`)
reproduces the per-deposit guards in `_drainDeposit` for one `eth_call` each,
instead of one wasted Groth16 per tick. The decision table is
`services::pipeline::deposit_preflight::classify`, kept pure so it is testable
without a node:

* **`public_in` over `uint48`** — `_drainDeposit` bounds it before narrowing,
  so it reverts however it is replayed. Quarantined; the deposit's own fields
  prove it.
* **empty escrow slot** — zero is the contract's "no pending deposit" sentinel:
  canceled, or flushed by someone else with the indexer still catching up.
  Dropped from this batch and nothing more — it resolves on its own, so it is
  not held against the deposit.
* **digest mismatch** — the replayed fields cannot hash to the slot, so the
  deposit is quarantined. But only once some deposit on this pool *has*
  matched: until then a mismatch is likelier a bug in the local derivation, or
  a misconfigured `pool_address`, than a bad deposit — and acting on it would
  take out the whole mempool. Until that proof arrives the mismatches are still
  dropped from the batch, which costs an `eth_call` and no proof.
* **the fee leaf does not pay us** — not ours to decrypt, malformed, or worth
  less than this flush costs. Never a fault: `flushBatch` is permissionless, so
  the deposit remains flushable by the relayer it does pay and reclaimable by
  its payer. Deferred rather than skipped, since only the payer or a change in
  gas will alter the verdict; see below.
* **the flush cannot be priced** — an asset this relayer does not take, or an
  oracle that is down. The relayer's problem, not the deposit's, so it is
  dropped from this batch and reconsidered on the next tick, without deferral:
  the condition is chain-wide and clears on its own.

An RPC failure aborts the tick rather than rejecting anything. A deposit must
never be judged unflushable because the node was unreachable.

### Failure budget

What pre-flight cannot classify is bounded by attempt count
(`flush_max_attempts`, `services::pipeline::deposit_failures`). Only reverts,
contract rejections and prover errors are charged — an RPC outage or a busy
prover must not quarantine the mempool.

A failed batch of more than one deposit names no culprit, and charging all of
them would quarantine the innocent majority. So nothing is charged; instead the
worker drops to one deposit per tick until a failure identifies itself, then
counts against that deposit alone. Any success clears the counts and restores
full-size batches, on the grounds that a chain that can flush at all was
probably never the deposits' fault.

Skipping a deposit is safe: it is not lost funds, because the payer can still
reclaim it with `cancelDeposit` after `cancelDelay`.

### Deferral

A deposit whose fee leaf does not pay this relayer is neither a fault nor
transient: re-judging it every tick reaches the same verdict, and because the
queue is ordered by age it holds a slot in the batch window while it does. A few
such deposits at the head therefore starve every payable deposit behind them,
indefinitely — until their payers cancel.

So pre-flight defers them (`Verdict::Defer`) instead. A deferred deposit is held
out of the window for a number of ticks that doubles with each consecutive
deferral, from two up to a cap of 64, and is then judged from scratch: one that
became payable — gas fell, or its asset re-priced — is picked up within minutes,
while one nobody intends to pay for costs a judgement once every 64 ticks. Being
judged flushable clears the backoff.

Deferral only removes ids from the batch window; on its own it would still leave
them at the head of the query. `DepositMempool::pop_pending` therefore pages
forward past every excluded id — quarantined and deferred alike — up to a bounded
number of rows per tick, rather than reading the oldest `flush_max_n` rows and
filtering afterwards. A backlog deeper than that bound is walked over successive
ticks: the deposits ahead of it stay excluded, so each tick resumes where the
last one was cut off.

### State

The ledger is re-read every tick, so a restart resumes from whatever the indexer
has recorded. The only in-process state is what the worker refuses to batch — the
quarantine set and the deferrals — which a restart therefore re-admits:
deterministic rejections are caught again by the next tick's pre-flight, counted
ones have to re-earn their attempts, and a deferred deposit is re-judged once and
deferred again. That is the intended trade for keeping the worker free of tables
of its own.

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

Where a chain collects a [shielded fee](#shielded-fees), the quote also carries
each token's MASP `assetId`, its `scale`, and `circuitAmount` — the base-unit
amount rounded **up** to a whole circuit unit, which is the exact `value` to put
in the fee note. Rounding happens here rather than in the client because
rounding down underpays by up to one whole unit and is refused, and because two
implementations of the same rounding drift apart.

A quote is advisory. It is neither signed nor stored, and the relayer re-derives
the requirement when the spend actually arrives — so nothing shaped like a quote
can be presented back to it as one.

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
| `flush_max_n` | no | 4 | Clamped to `MAX_DEPOSITS_PER_BATCH = 4` (`MAX_L_BATCH = 8`, two leaves per deposit) |
| `flush_max_attempts` | no | 5 | Attributable failures before a deposit is skipped. `0` disables quarantine |
| `native_adapter_address` | no | — | Enables `withdrawNative`. The SNARK must name it as both `recipient` and `relayer` — the adapter is the pool's caller there |
| `swap_wrapper_address` | no | — | Enables `/v1/swap` |
| `native_symbol` | no | `ETH` | Oracle base for the native gas token |
| `native_decimals` | no | 18 | Must be ≤ 38 |
| `fee_markup_bps` | no | 1000 | 10%. Must be ≤ 1_000_000 |
| `swap_default_deadline_s` | no | 300 | Applied when the wallet pinned no deadline. Without a bound a swap can sit in the mempool and execute at an arbitrarily later price with only `min_out` protecting the user |
| `accepted_fee_tokens` | no | `[]` | `{symbol, address, decimals, quote_symbol}`; decimals ≤ 38 |
| `shielded_fee_address` | no | — | bech32m address the relayer is paid at. **Setting it makes a fee mandatory** — see below |
| `shielded_fee_ivk` | no | — | Incoming viewing key for that address, big-endian. Must be set together with it. Normally from the environment, not the TOML |
| `shielded_fee_grace_bps` | no | 300 | How far below the submit-time quote a payment may fall. Must be < 10 000 |
| `shielded_fee_assets` | no | `[]` | Asset ids accepted as fees. Empty means every token in `accepted_fee_tokens` |
| `public` | no | — | Wallet-facing block, served verbatim by `/chains`: `name`, `rpc_url`, `tree_depth`, `permit2_address`, `explorer_url` |

Per-chain env overlay:
`RELAYER_CHAIN_<id>_{POOL_ADDRESS,RPC_URL,SIGNER_KEY,SWAP_WRAPPER_ADDRESS,NATIVE_ADAPTER_ADDRESS,NATIVE_SYMBOL,FEE_MARKUP_BPS,SHIELDED_FEE_ADDRESS,SHIELDED_FEE_IVK}`.

⚠️ The overlay only rewrites chains **already declared** in the TOML. A variable
naming a chain with no `[[chains]]` block is silently discarded.

`stack/config/prod/relayer.toml` ships a zero `signer_key_hex`, which fails
secp256k1 validation at startup — the prod profile needs
`RELAYER_CHAIN_<id>_SIGNER_KEY` set to boot.

## Shielded fees

By default the relayer pays gas out of its own signer and charges nothing;
`/v1/spend/estimate` quotes a fee that nothing collects. Setting
`shielded_fee_address` turns collection on for a chain.

The payer funds one of the transact circuit's three output slots with a note
addressed to that address. It rides in the `aux` the relayer already receives,
so there is no extra request, no extra calldata, and — the point — no on-chain
transfer linking the payer to the spend. The relayer trial-decrypts each output
with its viewing key, rebuilds `cm = Poseidon(asset·2^64 + value, pk, rho, rcm)`
over its own `pk`, and accepts the value only if that equals the `out_cm` the
proof committed to. A note encrypted to the relayer but owned by someone else,
or one whose plaintext inflates the value, fails there.

Consequences worth knowing before enabling it:

- **It is all or nothing.** Presence of the key in `/chains` is the contract:
  once configured, every `/v1/spend` and `/v1/swap` on that chain must carry a
  sufficient fee, and one that does not is refused **402**. A wallet that does
  not yet build fee outputs will be refused every spend.
- **A fee may be split across slots.** Outputs paying the relayer are summed,
  so a payer is credited for all of them. They must name one asset: a payment
  spread over several has no single price to check it against, and is refused.
- **A fee consumes an output slot.** `TRANSACT_OUT` is 3 and fixed by the
  circuit, so the fee replaces a change slot: a transfer goes from
  `[recipient, change, change]` to `[recipient, change, fee]`.
- **The fee is paid in the asset being spent**, because a spend is built in a
  single asset. An asset this relayer will not take as a fee therefore cannot be
  relayed at all, not merely cannot pay for itself.
- **`accepted_fee_tokens` is the effective list**, whatever
  `shielded_fee_assets` says. The two are ANDed: the allowlist can only narrow,
  never widen, because an asset the fee table cannot price has no quote to check
  a payment against. Leaving `shielded_fee_assets` empty means "no extra
  restriction" — *not* "every registered asset". `/chains` publishes exactly the
  intersection, so an asset it advertises is one a submission will accept.
- **`prover.transact_vkey_path` is required.** Without it a wallet's proof is
  not checked before submission, so `out_cm` and `nullifier[0]` — the values a
  fee is bound to — are unverified. The relayer refuses to boot in that
  combination rather than enforce something that does not hold.
- **The viewing key cannot spend.** `ivk` recognises payments and reads their
  values; moving them needs `nsk`, which never has to exist on this host.
  Generate the pair with the SDK's `buildSpendingKey` + `encodeAddress` and keep
  `nsk` elsewhere. It is still a secret: `FeeRecipient` and `ShieldedFeeChecker`
  hand-write `Debug` to redact it, and a test pins that. Do not derive it.
- **Quotes are not signed or stored.** The requirement is re-derived when the
  spend arrives; `shielded_fee_grace_bps` is what absorbs the gas-price and
  oracle drift between the estimate and the submission.

Clients read the terms from `/chains` (`shieldedFee`) and the live amount from
`/v1/spend/estimate` (`fees[].circuitAmount`, already rounded up to a whole
circuit unit, plus `shieldedFeeAddress`). On the wallet side the SDK's
`bundle → feeOutputFromEstimate` turns that response straight into an output
slot.

## Replicas

**Do not run more than one replica per chain.** Unlike the indexers there is no
advisory lock here: the tree mirror, the nullifier guard, and the idempotency
cache are all per-process state, and two relayers on one chain would reserve
overlapping leaf ranges and race each other's `flushBatch`.

## Layering

Standard binary layout, plus `services/pipeline/` (spend, swap and flush over a
shared `common`, with the flush worker's decision table in `deposit_preflight`
and its attempt bookkeeping in `deposit_failures`) and `services/tree/`.
`build.rs` stamps the version and git SHA that `/health` reports. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
