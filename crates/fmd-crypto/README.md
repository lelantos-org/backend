# fmd-crypto

Fuzzy Message Detection primitives, plus the Merkle tree the whole system
shares. Pure computation — no IO, no database, no config.

Everything here has an exact counterpart elsewhere and must stay byte-identical
to it: the SDK's TypeScript (`sdk/src/crypto/`) and the circuits
(`circuits/src/lib/`). A change that is merely "equivalent" is a break.

**Privacy gate.** `explorer-indexer` and `explorer-webserver` must never depend
on this crate — they serve public analytics, and reaching detection primitives
from that side is what would let a public endpoint answer a private question.
The rule is recorded in those crates' `Cargo.toml` and `lib.rs`. It is a
convention today, not a build failure: nothing in
[`.github/workflows/rust.yml`](../../.github/workflows/rust.yml) enforces it, so
a new dependency edge has to be caught in review.

## Modules

| Module | Contents |
|--------|----------|
| `clue` | FMD scheme `lelantos.fmd.v4`: point coords, `pack`/`unpack`, `test_clue`, `test_clue_batch` |
| `filter` | Detection-key parsing and the `fmd-indexer`-facing clue tests |
| `poseidon` | circomlib-compatible Poseidon, arities 1–12 (state width 2–13), plus the sparse variant |
| `babyjubjub` | Scalar mul, public key from secret key, byte conversions |
| `tree` | Quaternary sparse Merkle tree with Poseidon-arity-5 nodes |

## Detection

A clue is a Baby-Jubjub point `R` plus `gamma` packed bits. Testing it against a
detection key is, per bit: one scalar mul, one Poseidon-6, and a Legendre symbol
that turns the hash into a bit. A key matches when every one of its `gamma` bits
agrees, so the false-positive rate is `2^-gamma`.

```rust
// One clue against one key.
fmd_crypto::test_clue(&detection_key, r_x, r_y, clue_bits, gamma);

// Hot path: parse the key once, test many clues against it.
let dk = fmd_crypto::filter::parse_detection_key(&detection_key, gamma)?;
fmd_crypto::filter::test_clue_parsed(&dk, r_x, r_y, clue_bits, gamma);
```

`parse_detection_key` returns `None` when the key is not exactly `gamma * 32`
bytes. `test_clue` treats that as a non-match rather than an error — a
malformed subscription key must not fail the batch that happens to contain it.

## Merkle tree

`tree::MerkleTree` is quaternary (arity 4) with nodes
`Poseidon(TAG_MERKLE, c0, c1, c2, c3)`. It takes leaves already hashed; the
`Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)` leaf hash itself is *not* here —
`fmd-webserver` (`services::poseidon`) and `relayer` (`services::tree`) each
define `TAG_LEAF` locally, so a change to the tag has to be made in both.
Levels are materialised, so
`root()` is O(1) and an absent node is indistinguishable from a
materialised all-zero subtree (`zeros[d+1] = hash(zeros[d] × 4)`).

The tree is *public* data — it lives here only because it shares the Poseidon
dependency and is consumed exclusively by FMD-zone crates. `fmd-webserver`
serves `/v1/tree-state` from it; `relayer` builds `tree_update_batch` witnesses
(frontier + path indices) from its own mirror of it.

## Tests and benches

`tests/fmd_vectors.rs` replays [`tests/vectors/fmd.json`](../../tests/vectors/fmd.json)
— shared with the SDK, which is what pins cross-language agreement rather than
merely testing this implementation against itself.

```sh
cargo bench -p fmd-crypto --bench clue_breakdown   # per-bit cost: scalar mul / Poseidon / Legendre
cargo bench -p fmd-crypto --bench filter_batch     # whole batched filter path
```

`clue_breakdown` is what the workspace's `lto = "thin"` release profile was
tuned against; see the comment in the root [Cargo.toml](../../Cargo.toml) for
the measured numbers.

## Layering

May import arkworks and rayon. Must NOT import `database`, any binary, or any
service crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
