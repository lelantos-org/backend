# groth16

Native Groth16 over BN254, against snarkjs-produced artifacts. Proves
`tree_update_batch` for the relayer, and verifies any snarkjs proof against any
snarkjs verification key.

A leaf library: it imports no other internal crate, touches no database, RPC
endpoint or HTTP framework, and has its own error type. That isolation is the
point — see [Why a crate](#why-a-crate).

## Modules

| Module | What it owns |
|--------|--------------|
| `witness_calc` | `WitnessCalculator`: evaluates the circom witness-calculation graph (`.wcd`), parses the resulting `.wtns` |
| `zkey` | `read_zkey`: the snarkjs zkey parser, vendored — proving key plus the A/B/C matrices |
| `prover` | `Groth16Prover`: witness to proof, serialised behind a single permit |
| `verifier` | `Groth16Verifier`: a prepared verification key, and a proof check against it |
| `error` | `Groth16Error`, split by whose fault a failure is |

`qap` is a seventh module, compiled only under `cfg(test)`: the vendored snarkjs
R1CS-to-QAP reduction, kept as an oracle now that the prover uses
`taceo-groth16`'s. See [Proving](#proving).

## Proving

`ark-groth16` supplies the types, the verifier and `PreparedVerifyingKey`, but not
the prover: `Groth16Params::prove` calls `taceo-groth16`, measured at ~1.7x on this
circuit (113k constraints, 2^17 domain, 16 threads).

The gap is entirely in the MSMs, which are **86%** of proving time here — the six
size-2^17 FFTs are the other 14%. Two things account for it. `ark-ec` 0.6 splits an
MSM into `num_threads / 2` chunks and then sizes the Pippenger window from the
*chunk* rather than from the input, so a bigger machine picks a smaller window and
does more bucket work; it also builds a `rayon::ThreadPoolBuilder` per chunk, inside
the hot loop. `taceo-ark-algebra` does neither, accumulates buckets in affine form
with batched inversion above `c >= 10`, and runs all five MSMs concurrently.

A Groth16 proof is a deterministic function of `(pk, r, s, witness)`, so the two
implementations must agree coordinate-for-coordinate on the same inputs.
`prover`'s `measure_phase_split` holds `r` and `s` fixed and asserts exactly that,
which is why the vendored `qap` reduction is still here: two independent routes to
the same proof is a stronger statement than either verifying alone.

It also checks the witness length against the zkey's `n_vars` and refuses a
mismatch, where `ark-groth16` silently truncated to the shorter of the two. A
witness that does not match the proving key cannot produce a verifying proof, so
failing at the call is better than failing at the pairing check.

## Boundary

Nothing arkworks is nameable by a caller. Everything crosses as plain data:

- **Witness in** — `TreeUpdateBatchWitness`, decimal field-element strings in the
  shape `tree_update_batch.circom` declares. The caller composes it; the widths
  belong to whoever builds it, and `TreeUpdateBatchWitness::signals()` returns the
  flattened name-to-decimals view so those widths can be pinned by a test without
  a zkey.
- **Proof out** — `TreeUpdateBatchProof`, decimal strings in snarkjs proof shape.
- **Proof in** — `SnarkjsProof`, the same shape, as it arrives out of JSON. Taken
  as strings rather than parsed points because parsing is exactly the step that
  must tell a caller's malformed input from an operator's broken key.
- **Public inputs in** — one big-endian `[u8; 32]` per public signal, in the
  order the circuit declares them.

The proving half is circuit-specific and says so. The verifying half is not:
`Groth16Verifier::load(path, public_signals)` takes any snarkjs key, and arity is
the only property of the caller's circuit it knows.

No arkworks or `num-bigint` type is nameable from outside. `WitnessCalculator`,
the zkey parser, the QAP reduction and the signal-to-`BigInt` parse are all
private: a caller that could reach them would pin this crate's ark version into
its own dependency graph, which is the thing the split exists to prevent.

## Errors

`Groth16Error` exists so a binary can map failures onto whatever it reports, and
the split is the reason it is not one opaque string. The same curve-point code
parses a verification key this deployment ships and a proof a caller sent:

| Variant | Whose fault | Typical mapping |
|---------|-------------|-----------------|
| `Prove` | ours | 500 |
| `Busy` | neither — backpressure | retried by the caller's next tick |
| `Key` | the operator's | 500 |
| `Verify` | ours | 500 |
| `InvalidProof` | the caller's | 400 |

Reporting a broken key as a bad request would blame whichever caller arrived
first; reporting a malformed proof as an internal error would hide the fix from
the only party who can apply it.

## Concurrency

Proving is serialised — the MSMs already saturate the machine, and two
concurrent proofs make both slower rather than either faster. The gate is a
`Semaphore` held *outside* `spawn_blocking` rather than a mutex inside it:
queueing on a blocking-pool thread would let a burst of requests park the whole
pool, which the database and everything else also draw from.

`Priority::Spend` queues for the permit; `Priority::Flush` takes it only if free
and yields `Busy` otherwise, so a background flush cannot make an HTTP caller
wait a whole proof. `is_busy()` lets a flush worker bail out before taking any
other lock.

## Why a crate

This is where arkworks 0.6 lives, and it links nothing else internal, so it
cannot meet the arkworks 0.4 that `light-poseidon` pins for `crypto`. The two
versions coexist because no crate reaches both, which the layering rule in
[ARCHITECTURE.md](../../ARCHITECTURE.md) states and this crate's dependency list
enforces. It also keeps the git-pinned `circom-witnesscalc`, the vendored zkey
parser and the ~50 MB-key proving path out of every binary that only wants to
submit a transaction.

`ark-circom` is deliberately absent: it is a hard `wasmer` dependency on every
target, for the circom wasm witness generator this crate does not run. The only
two things it provided — the zkey parser and the QAP reduction — are vendored
here.

## Tests

`cargo test -p groth16`. Two are conditional:

- `verifier`'s published-key test skips unless
  `circuits/build/4x6_verification_key.json` exists.
- `tests/zkey_compat.rs` proves a published golden vector with a real zkey and
  dumps `proof.json` / `public.json` in snarkjs shape, so the result can be
  checked with the snarkjs CLI. Skipped unless `ZKEY_COMPAT_DIR` points at a
  directory holding `tree_update_batch.wcd`, `tree_update_batch_final.zkey` and
  `vector.json`.
