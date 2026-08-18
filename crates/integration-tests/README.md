# integration-tests

Cross-crate end-to-end tests. The crate root is empty on purpose — everything
lives in `tests/`, and every dependency is a **dev**-dependency, so nothing here
can be linked into a shipped binary.

It is the one place allowed to import several binary crates at once: the
[layering rules](../../ARCHITECTURE.md) forbid binary → binary everywhere else,
which is what makes a test that drives `ingester` → `fmd-indexer` →
`fmd-webserver` in one process impossible to write inside any of them.

## Run

```sh
cargo test -p integration-tests
```

Needs a working Docker daemon: the harness starts a real Postgres via
testcontainers rather than mocking the database, because most of what these
tests pin is SQL behaviour.

## Harness

One container per test binary, behind a `OnceCell`, with migrations applied
once. Each test then takes a process-wide mutex (`serial_lock`) and truncates
every table before running — the tests share a database and assert on absolute
row sets, so they cannot interleave.

## What is covered

| Test | Pins |
|------|------|
| `fmd_consume_pairs_root_advanced_with_note_created` | consume holds an escrowed note pending until the batch committing it lands |
| `explorer_consume_writes_tree_advances` | `RootAdvanced` projects old root / new root / leaves inserted |
| `nullifier_chunk_feed_slices_spent_set` | chunk boundaries and the 10-byte truncation of the spent-set feed |
| `commitment_chunk_serves_only_a_prefixed_hex_leaf_hash` | the feed serves one `0x`-prefixed `leafHash` and *not* the raw `cm` / `cv_dep` |
| `list_matches_returns_only_the_requested_chains_notes` | a subscription spans every chain, so `matches` must be filtered by `chain_id` |
| `asset_metadata_write_leaves_the_column_it_omits_alone` | a partial `AsChangeset` write must not NULL the column it does not carry |

Each of those is a property whose failure is silent in production — a wallet
inflating its balance with an unspendable note, a client hashing a decimal
string as if it were hex — which is why they are pinned here rather than left
to review.

Crate-local tests live next to their crate (`ingester/tests/synthetic_blocks.rs`,
`fmd-indexer/tests/fixture_replay.rs`, `explorer-indexer/tests/fixture_replay.rs`,
`risk-webserver/tests/screen.rs`) and also use testcontainers. `just test` runs
everything.
