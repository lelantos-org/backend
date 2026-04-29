# Lelantos Backend

Rust workspace of backend services for Lelantos: EVM log ingestion, FMD (fuzzy message detection) indexing, explorer APIs, tree-advance relaying, and quote aggregation for shielded swaps.

## Crates

| Crate | Description |
| --- | --- |
| `shared` | Common primitives: entities, shutdown, tick driver, config loader, tracing init |
| `chain-types` | ABI types and decoding (no DB, no IO) |
| `fmd-crypto` | FMD primitives: Poseidon, Baby Jubjub, filter, tree |
| `database` | Diesel schema, migrations, bb8 pool, cursor repository |
| `ingester` | Live and backfill EVM log ingester (per chain) |
| `fmd-indexer` | FMD consume and filter workers |
| `explorer-indexer` | Asset and tree-advance indexer |
| `fmd-webserver` | FMD HTTP API: notes, matches, subscriptions, tree |
| `explorer-webserver` | Explorer HTTP API: assets, flows, tree-advances |
| `relayer` | Tree-advance prover and submitter |
| `metaquoter` | Quote aggregation for shielded swaps (DB-less) |
| `integration-tests` | End-to-end tests via testcontainers |

See [ARCHITECTURE.md](ARCHITECTURE.md) for layering rules and conventions.

## Requirements

- Rust 1.95 (pinned via `rust-toolchain.toml`)
- [`just`](https://github.com/casey/just)
- Docker (local stack, integration tests)

## Development

```sh
just build    # cargo build --workspace
just test     # cargo test --workspace
just ci       # fmt + clippy + test
```

## Local stack

`stack/` runs the full system with Docker Compose: Postgres, an ephemeral Anvil node, contract deployment, and all services.

```sh
cd stack
just up               # everything (profile=all)
just up-profile fmd   # single profile: db | anvil | ingester | fmd | explorer | relayer | metaquoter
just logs <service>   # tail one service
just down             # stop + wipe volumes
```

The relayer needs circuit artifacts; `just up` fetches them automatically (`just fetch-circuits` to run manually). Service configs live in `stack/deploy/*.toml`.

## License

MIT OR Apache-2.0
