# Remaining sync-cost optimizations

Work not yet implemented, with the corrections learned while implementing the
parts that are.

## Already landed (baseline for what follows)

- **Tick catch-up.** `shared::tick::TickProgress` + `shared::backoff::Backoff`.
  The driver sleeps only when idle, so initial sync is bounded by Postgres
  rather than by `batch / tick_ms` (previously 2,000 events/s at the documented
  defaults — ≥9 minutes for a 2²⁰ backfill in consume alone, and again in
  filter). `*_tick_ms` is now the idle **ceiling**, not a fixed period.
- **Packed ephemeral point.** `NoteOut` / `MatchOut` carry
  `ephPubPackedHex` instead of `ephPubX`/`ephPubY`, and `clueBitsHex` is gone
  (it was re-derivable from `ciphertext[0..2]`, which the client already
  receives). ~33 MB off a full-tree cold sync on the default strategy.
- **`ensure_dense`** in `services/commitments.rs`, and `upsert_monotonic` now
  reports whether it wrote so the advisory-locked consume loop can log a lost
  race instead of swallowing it.

---

## Corrections to carry forward

Three things assumed during planning turned out to be wrong. They change the
work below, so fix them before quoting the original rationale at anyone.

1. **Reorgs *are* handled.** `fmd-indexer`'s README lists "no reorg handling"
   as a known gap, but `database::reorg::apply_pending` is wired into both
   consume ticks and does its own retraction. What is genuinely dead is
   `NotesRepo::delete_from_block` / `SpentNullifiersRepo::delete_from_block` —
   defined, implemented, never called. **The README needs correcting, and the
   freeze rule in §2.4 must be re-derived against what actually exists.**

2. **Point packing is circomlibjs, not arkworks — and little-endian.**
   `sdk/wasm/jubjub/src/curve.rs::compress` writes `y` as 32 bytes **LE**, then
   sets the high bit of byte 31 iff `x > (q-1)/2`, from a *vendored* curve impl.
   Not `ark-ed-on-bn254::serialize_compressed`. Every other hex field on this
   wire is big-endian, so the packed field is the sole exception and is
   commented as such at both ends. Relevant below only as a warning: assume
   nothing about encodings without reading the implementation.

3. **The SDK already hardens nullifier chunks.** `assertWellFormed`
   (`sdk/src/wallet/nullifier-store.ts`) checks chunk id, over-length, and
   complete-but-short. §3.3's "add a `chunk_id == requested` check" is done.

---

## Part 2 — Chunked tree mirror

Fixes an unbounded cold start. `services/tree.rs` calls
`list_leaf_inputs_from(pool, chain_id, from)`, and `repositories/notes.rs` has
**no LIMIT**. After a restart `from = 0`, so the first `/v1/tree-state` loads
every note row into memory and hashes the whole tree — ~1M leaf hashes plus
349,525 internal Poseidon-4 calls — **while holding the per-chain mutex**.
`app/cache.rs` already documents the steady-state cost; the cold-start case is
unbounded.

Geometry: depth 10, **arity 4**, `4^10 = 1,048,576` leaves, 349,525 internal
nodes. Chunk = 1024 leaves = a depth-5 quaternary subtree.

### 2.1 Tree construction moves to `fmd-indexer`

**The load-bearing decision, and it changes a crate boundary.** Persisted tree
state has exactly one safe writer, and `fmd-webserver` cannot be it:

- The webserver builds the mirror lazily, per request, in a per-process moka
  cache. Every replica builds its own. Once that state is *persisted* rather
  than cached, N replicas racing on it is a write conflict with no arbiter —
  the webserver has no advisory lock and no leader election.
- `fmd-indexer` already is the single writer for `notes`, already holds the
  per-chain advisory lock (`adapters::locks::ChainLocks`, `NS_FMD_CONSUME`),
  and already has automatic failover.

So the indexer builds and persists; the webserver becomes a **pure reader**.
Consequences, all simplifications:

- `services/tree.rs`'s `sync_mirror` / `append_leaves` / `TreeMirror` delete
  entirely, along with the per-chain `Mutex` and the `tree` moka cache.
  `/v1/tree-state` becomes a `SELECT` of the frontier blob and root.
- `services/commitments.rs` stops recomputing `leaf_hash` per request behind a
  64-entry cache and serves precomputed blobs. The `chunks` cache becomes
  redundant — Postgres plus the CDN cover it.
- `ensure_dense` moves to the writer, where a violation can stop the pipeline
  instead of failing one request.
- Cold start stops existing on the read path.

Cost: a new `TickService` in `fmd-indexer` (it should use `TickProgress` from
the start), and `/v1/tree-state` lags the indexer by one tick rather than being
computed on demand. That lag is already true in practice — the webserver
mirrors `notes`, which the indexer writes. **Check it against whatever the
relayer's `tree_update` witness builder assumes about freshness.**

### 2.2 Dense chunk representation

Store every node of a frozen subtree as a flat array in binary-heap layout,
generalised to arity 4: children of `i` are `4i+1..4i+4`, parent is `(i-1)/4`.
A Merkle path is then **array indexing with zero hashing**.

| | bytes | client hashing |
|---|---|---|
| today, leaves only | 33.6 MB | 349,525 Poseidon-4 |
| dense, all nodes | 44.7 MB (+33%) | **0** |
| dense, one note's chunk + upper domain | **85 KiB** | **0** |

Per chunk: `1 + 4 + 16 + 64 + 256 + 1024 = 1,365` nodes = 43,680 B (vs 32,768 B
leaves-only). Upper domain: 1,024 chunk roots (32,768 B) + 341 internal nodes
(10,912 B).

Build the array as a byproduct of appending — write each parent as the carry
folds upward, with a `dirty: Vec<usize>` recording changed indices. No separate
pass.

> **The 85 KiB row is not in scope here.** It is 384× and it is
> `handlers/http/tree.rs:1-5`'s deliberately-absent endpoint with a 1,024-leaf
> fig leaf: the server learns which bucket your note is in, and a wallet with N
> notes leaks N buckets whose intersection over time is far more identifying
> than any one. Zcash accepts this tradeoff (`GetSubtreeRoots`, 2¹⁶ shards).
> Whether we do is a threat-model decision for whoever wrote that comment.
> **Dense chunks are a +33% bandwidth regression unless that call is made
> first** — §2.3's persistence wins stand on their own with leaves-only blobs.

### 2.3 Persistence

Three tables, per-chain:

- `commitment_chunks(chain_id, chunk_id, data BYTEA)` — frozen blobs, immutable
  once written.
- `active_chunk_nodes(chain_id, node_index, node_hash)` — the partial chunk
  only, UPSERT on the dirty set. A block of 50 notes writes ~250 rows, not
  1,365.
- `tree_indexer_state(chain_id, active_position, frontier BYTEA)` — the
  ommer/frontier stack, ≤ 10 records.

Boot: read state, read `max(chunk_id)`, three-way dispatch on `active_position`
vs `(max_chunk_id + 1) * 1024`, and in the mid-chunk case one
`SELECT node_index, node_hash ORDER BY node_index` into a pre-sized vec.
**Zero re-hashing** — reconstruction is a struct literal, because every
interior node of the partial chunk was persisted. Only the ≤1 block of leaves
since the last commit gets hashed. Frozen chunks never enter RAM; the mirror is
bounded by chunk size, not note count.

### 2.4 Freezing is one-way — bound it by reorg depth

A frozen blob and a folded frontier cannot be unwound without re-hashing from
the previous checkpoint. Since reorgs **are** handled (correction 1), an
already-frozen chunk containing re-orged leaves is unrecoverable state *today*,
not hypothetically.

Rule: **freeze chunk `c` only once its highest `block_number` is at least
`REORG_DEPTH` behind the ingester tip.** A chunk that is full but not yet
reorg-safe stays in `active_chunk_nodes` and is served as a tail chunk
(`max-age=5`, not `immutable`). One extra column, one comparison — and it is
what makes `immutable` on a frozen chunk *true* rather than hoped-for.

Derive `REORG_DEPTH` from what `database::reorg` actually tolerates, not from a
guess.

### 2.5 Two-phase commit

A shadow working copy with `commit` / `rollback`:

1. Append leaves into the **shadow**, computing the write set (dirty nodes,
   freezes, new frontier). Roll back on `Err`; do **not** commit on `Ok`.
2. One Postgres transaction: insert frozen blobs, upsert active nodes, update
   state. All three or none.
3. Promote the shadow only after the DB commit succeeded.

DB-atomic, then RAM-atomic, then cursor. A failure anywhere rolls the RAM tree
back and the retry is idempotent, because the cursor never moved. Wrap the leaf
hashing in `block_in_place`.

Later, the committed side can become an `ArcSwap` — it is only ever replaced
wholesale, so readers stop blocking the writer and every reader sees a
whole-block-consistent snapshot.

**Two failure modes to design out** (both are live bugs in the reference
implementation this design is taken from):

- A batch straddling a chunk boundary must still persist the *new* chunk's
  dirty nodes. Skipping them while recording a position past the boundary makes
  restart reconstruct phantom empty leaves and fold a wrong root upward.
  Silent, and only on a crash in that window.
- The truncate-on-freeze and the state update must be in **one** transaction.
  A bare auto-committing truncate plus an in-memory position move has the same
  corruption shape.

---

## Part 3 — Binary chunk feeds

Both chunk feeds ship hex inside JSON. There is no `CompressionLayer` at the
origin; Cloudflare gzips JSON at the edge and will not compress
`application/octet-stream`, so the honest comparison is gzipped-JSON vs raw
binary: **−31% nullifiers, −25% commitments**. The larger win is CPU — a cold
sync currently does ~1M JSON string parses and ~2M `BigInt("0x…")` calls.

### 3.1 Wire format

Shared 16-byte header, big-endian scalars:

| off | size | field |
|---|---|---|
| 0 | 4 | `magic` = `"LCHK"` |
| 4 | 1 | `version` |
| 5 | 1 | `kind` — 1 = nullifier, 2 = commitment |
| 6 | 1 | `flags` — bit0 `is_complete`; rest reserved, decoder rejects non-zero |
| 7 | 1 | `param` — codec parameter (`k` for kind 1) |
| 8 | 4 | `chunk_id` |
| 12 | 4 | `count` |

`chunk_id` and `is_complete` travel in the **body**, not headers: `is_complete`
is a termination condition, and a stripped header means a client pages to
`MAX_CHUNKS` or treats a tail as complete. The magic check is also what catches
a stale cached JSON body — JSON starts with `{`, so it fails loudly.

**Kind 2, commitments** — two shapes, keyed on `is_complete`, because a partial
subtree has no meaningful dense form:

- complete (`count == 1024`): the full 1,365-node dense array, 43,680 B fixed
  (only if §2.2's dense decision is made; otherwise 32,768 B of leaves).
- tail (`count < 1024`): `count × 32` B of leaves only. At most 1,023 leaves and
  `max-age=5` anyway, so precomputing internal nodes for a shape that changes
  every block buys nothing.

`leaf_index` is **not** on the wire in either shape; entry *i* is leaf
`chunk_id * 1024 + i`. Both lengths are exact, so a wrong-shaped body fails the
length check rather than decoding into garbage.

**Kind 1, nullifiers** — Rice-coded gaps over the sorted truncated values, i.e.
BIP-158's Golomb-Coded Set with `M = 2^k`. Measured on uniform 80-bit samples,
with `L = 80 - log2(count)`:

| scheme | bits/entry |
|---|---|
| entropy floor | L + 1.443 |
| **Rice, k = L−1** | **L + 1.541** |
| Rice, k = L | L + 1.582 |
| Elias-Fano | L + 2.000 |

`k = L − 1`, not `L` (Wuille's rule, cited normatively by BIP-158). Expected
body 9,174 B; **max body 9,232 B**, derivable because
`Σ⌊gᵢ/2ᵏ⌋ ≤ ⌊v_max/2ᵏ⌋`, so the decoder still validates a hard bound before
allocating. Bitstream is **MSB-first** to match BIP-158 (enables cross-checking
against a reference implementation, and makes the unary scan a `clz32`) — which
differs from `sdk/src/core/bits.ts`'s LSB-first convention, so comment both
sites.

Sort in Rust on the `u128` truncated value, never in SQL (`ORDER BY nf` sorts
all 32 bytes — a different order). Leave the repository's `seq` ordering
untouched: the `seq` range still defines chunk membership, which is what keeps
chunks immutable. Duplicates need no special case (`g = 0` → `q = 0`); two
32-byte nullifiers *can* share their low 10 bytes, so never assert strict
monotonicity.

Do **not** gzip — the payload is incompressible by construction, and
`application/octet-stream` also avoids `Vary: Accept-Encoding`, so the CDN keeps
one object per URL.

### 3.2 Content-addressed tip

`/v1/tree-state` is `max-age=5` on the full body, so every wallet poll hits
origin **and** takes the per-chain mutex.

`GET /v1/tree-state` → `302` + `max-age=3` → `GET /v1/tree-state/{leaf_count}`
with `max-age=31536000, immutable`. The moving pointer is a tiny redirect; the
payload is permanently CDN-cacheable. `leaf_count` is monotonic and
deterministic, so each version is uniquely identifiable and verifiable by
recomputing the root. A `leaf_count` the origin has moved past must return
**404 with a short cache**, not a stale body.

**This one needs a Terraform change**, unlike the chunk feeds: `tree_state_expr`
in `infra/terraform/cache.tf` is *exact equality* on `/v1/tree-state`, so
`/v1/tree-state/12345` will not match. Change `eq` to `starts_with` for both it
and the `/fmd` variant, and land it before the handler ships.

The same trick retires the tail-chunk problem: content-address the incomplete
chunk by its element count and it becomes immutable too.

### 3.3 Mutually-referential state

`/v1/tree-state` and the commitment chunks are fetched separately, so a chunk
freezing between the two can hand a client state that cannot produce a valid
witness. **Check first whether `TreeStore.verifyRoot` currently masks this as a
retry or fails hard** — that decides whether this is a bug fix or a latency
optimisation. The fix, if needed, is one atomic body carrying tree size, upper
frontier, frozen roots, and the active chunk together.

### 3.4 Rollout

Routes stay on `/v1`; the bodies change in place and the JSON handlers are
deleted, not kept behind a shim.

The *cache key* must still change, and that is physics, not semver: complete
chunks carry `immutable` for a year in the **browser disk cache**, which no
server-side purge reaches. Serve new bytes at the same key and every existing
wallet reads JSON off disk, fails the magic check, and stops syncing until the
user clears site data.

Add **`?enc=1`** to the two chunk fetches. Different URL, different browser
cache entry, different Cloudflare cache key (`cache.tf` sets only
`ignore_query_strings_order`, which sorts rather than strips) — clean miss, no
purge. Preferred over a `.bin` suffix because the cache rule's predicate tests
`http.request.uri.path`, which excludes the query string, so **no Terraform
change**; and `cache_deception_armor` stays a no-op on an extensionless path.
*Verify that predicate during review — it is the one Terraform assumption the
chunk-feed rollout rests on.*

The server ignores `enc` entirely. Document in `router.rs` that any future
wire-format change bumps it. It is not a credential, so it must **not** join
`SECRET_PARAMS`.

Order: backend merge → SDK prerelease → e2e (submodule + prerelease; the only
real integration gate, since nothing else exercises both codecs against a live
Postgres) → SDK release → backend deploy → webapp-ui. Old clients break at the
backend deploy — keep the window short. Rollback stays clean: the
un-parameterised URLs were never re-cached under new content.

### 3.5 Client changes

`createJsonClient` already exposes `readonly raw: HttpClient` for non-JSON —
use it rather than adding a `getBytes`; cleanest is a `bytes()` method beside
`get`/`post`/`del` so it reuses the closed-over `url()` builder.

`CommitmentChunkOut.entries` becomes `leafHashes: bigint[]` outright.

**`nullifier-store.ts` carries a latent bug that this part makes live.** It
resumes mid-chunk *positionally*:

```ts
const fresh = chunk.nullifiers.slice(Math.max(0, this.syncedCount - base));
```

That assumes entry *k* of chunk *N* is global sequence `N*1024 + k`, which
holds only while the server returns `ORDER BY seq ASC`. Rice coding requires
sorting each chunk **by value**, and then a re-fetched tail chunk interleaves
new entries throughout rather than appending them — so the slice drops an
arbitrary set of *unseen* values, `syncedCount` advances past them, and nothing
revisits them. The wallet's spent set silently gains holes; `has()` returns
false for a spent note, and the wallet builds a spend the chain rejects.

Fix: fold every entry of the re-fetched chunk (the `Set` makes re-adding free
and idempotent), keep `syncedCount` as a pure chunk cursor rather than an array
index, and compute `added` from the set-size delta. Update the module header,
which states the now-false positional invariant.

In-memory representation stays `Set<bigint>`. A single global Elias-Fano merged
from all chunks is 8.13 MB vs ~75 MB with ~0.17 µs lookups — but **per-chunk**
EF is a trap, since `seq`-defined chunks all span the full universe and one
membership test becomes 1024 predecessor queries. Touch `spent` only through
`add`/`has`/`size` so the swap stays a one-file change.

---

## Deferred decisions

- **Dropping `chain_id` from `NoteOut`.** Not redundant: `ListNotesQuery.chain_id`
  is `Option<i64>`, so omitting it returns rows spanning every chain and the
  per-row value is the only discriminator. The SDK always passes it, but the
  endpoint is public. A small integer that gzips to nothing next to 32 bytes of
  incompressible point — ~1% of the win for real API narrowing. Left in place.
- **Chunk size.** Each doubling saves exactly 1 bit/entry (1024 → 16384 is
  −5.7%) and breaks nothing, but the tail chunk is `max-age=5`, so a
  steady-state poll would re-pull 139 KB instead of 9 KB. Left at 1024.
- **Range-partitioned chunks.** Chunking nullifiers by *value range* rather
  than `seq` takes the high 10 bits free from the URL: 62 bits/entry, 1.31 MB
  of 9.44 MB — twenty times every coding improvement combined. Rejected because
  a new nullifier lands in an arbitrary chunk by value, so every chunk mutates
  forever, killing `immutable` and the `syncedCount` cursor. Recovering both
  needs an epoch-cut content-addressed base snapshot plus a seq-chunked delta
  feed. Coherent, but not worth it for 1.3 MB on a feed a quarter the size of
  the commitment feed.

---

## Verification

**Part 2** — property test: append K leaves in random batch splits, assert the
root matches a from-scratch tree. Crash-injection: kill between DB commit and
RAM promote, assert restart reproduces the same root; kill mid-batch across a
chunk boundary, assert no phantom leaves. Assert boot after restart issues O(1)
queries and zero hash calls. Assert a gap in `leaf_index` stops the pipeline.

**Part 3** — round-trip property tests for `count ∈ {0, 1, 2, 1023, 1024}` plus
a duplicate-heavy set. **Cross-language known-answer vectors** in a checked-in
file, asserted by both the Rust and TS tests, each naming the other — the only
thing that catches an endianness or bit-order disagreement. (The packed-point
work already uses this pattern: `PACKED_BASE8` is hardcoded in both
`services::field::tests` and `client.test.ts`.) Include a vector with values
above 2⁵³, the `Number` corruption trap. Extend `integration-tests/tests/e2e.rs`
to assert header bytes **by hand** rather than via the reader, and assert
`body.len()` — the durable byte-size guard.

`nullifier-store.test.ts`: sync tail `[5n, 9n]`, re-sync as `[3n, 5n, 7n, 9n]`,
assert `size === 4`. **This fails against the current positional slice**, which
is what makes it worth writing.

Byte measurement: `curl -s -o /dev/null -w '%{size_download}' -H
'accept-encoding: gzip'` before/after — explicit gzip, since the meaningful
"before" is the compressed number.

Gates, per `AGENTS.md`: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets`, `cargo test --workspace`, `npm test`.

---

## Sequencing

| order | part | depends on |
|---|---|---|
| 1 | Correct the README's reorg claim; re-derive §2.4 | — |
| 2 | **Part 2** — chunked tree mirror | uses `TickProgress` |
| 3 | **Part 3** — binary chunk feeds | Part 2 (soft; leaves-only works without) |

Part 2 is the larger and moves a crate boundary. Part 3 is mostly mechanical
once the storage exists, but its SDK half — a hand-written Rice decoder plus
the `nullifier-store` fix — is where the risk sits.
