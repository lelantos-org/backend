# Lelantos backend stack

Docker Compose stack that runs the full backend locally: Postgres, an ephemeral
Anvil node, a one-shot contract deploy, and every Rust service.

## Quick start

```sh
just up                 # everything (profile=all)
just up-profile fmd     # one slice only
just logs relayer       # tail a service
just down               # stop and wipe volumes
just --list             # all recipes
```

Requires `docker`, `just`, and `gh` (for circuit artifacts). First run downloads
~200 MB of proving keys and builds the Rust images.

For what each service actually does and how it is configured, see its own
README — linked from the [backend README](../README.md#crate-readmes).

## Layout

```
config/dev/     service TOMLs for the local anvil stack   (mounted by default)
config/prod/    deployment templates, real chains         (STACK_ENV=prod)
scripts/        deploy-contracts.sh, fetch-circuits.sh, lib.sh
circuits/       downloaded proving artifacts              (gitignored)
docker-compose.yml
justfile
```

## Services

| Service | Port | Profiles |
|---|---|---|
| `postgres` | 5432 | `db` `ingester` `fmd` `explorer` `relayer` `risk` `all` `prod` |
| `anvil` | 8545 | `anvil` `relayer` `all` |
| `deploy` (one-shot) | — | `all` |
| `ingester` | — | `ingester` `fmd` `explorer` `all` `prod` |
| `fmd-indexer` | — | `fmd` `all` `prod` |
| `fmd-webserver` | 3001 | `fmd` `all` `prod` |
| `explorer-indexer` | — | `explorer` `all` `prod` |
| `explorer-webserver` | 3002 | `explorer` `all` `prod` |
| `relayer` | 3003 | `relayer` `all` `prod` |
| `risk-webserver` | 3004 | `risk` `all` `prod` |
| `metaquoter` | 8081 | `metaquoter` `all` `prod` |

Select a profile with `just up-profile <name>` or `just PROFILE=<name> <recipe>`.
`db` is Postgres alone, for running `cargo test` against a real database.

## Configuration

`STACK_ENV` selects which config directory the services mount, defaulting to
`dev`:

```sh
just up                    # config/dev/  — anvil, chain 31337
STACK_ENV=prod just up     # config/prod/ — mainnet templates
```

Four services mount a TOML from that directory — `ingester`, `explorer-indexer`,
`relayer`, and `metaquoter`. The rest are configured entirely through
environment variables in `docker-compose.yml`.

The chain-aware services read their TOML and then overlay per-chain environment
variables named `<SERVICE>_CHAIN_<id>_<FIELD>` (see `shared::config_env`).
Deployed addresses, RPC URLs, and signer keys arrive that way rather than being
baked into the file.

> **The overlay only rewrites chains already declared in the TOML.** A
> `RELAYER_CHAIN_31337_POOL_ADDRESS` with no matching `[[chains]]` block is
> silently discarded — no warning, no error. When adding a chain, add it to the
> TOML first.

`config/prod/relayer.toml` ships a zero `signer_key_hex`, which fails secp256k1
validation at startup; `RELAYER_CHAIN_42161_SIGNER_KEY` must be set for that
profile to boot.

## Contract deployment

Under profile `all`, the one-shot `deploy` service runs before the backends:

1. `forge script DeployTest.s.sol` — verifiers, MASP, mock tokens, `NativeAdapter`
2. `forge script DeployTestSwap.s.sol` — `UniV3Adapter`, `SwapWrapper`, swap mocks
3. `forge script DeployTestYield.s.sol` — a `MockERC4626` vault and its
   `ERC4626Venue` per asset, registered as new yield ids
4. Funds `FUND_RECIPIENT` with native coin, WETH, and two mock ERC20s
5. Writes `/addresses/addresses.env` to the shared `addresses` volume

Every backend's entrypoint sources that file before exec'ing its binary, which
is how the freshly deployed addresses reach the per-chain overlay.

Re-running the deploy mints new addresses (new nonces), so backends must be
restarted to pick them up — `just redeploy` does both.

A yield id sits *alongside* the token's plain id rather than replacing it: ids
1,2,3 stay risk-free custody, and 4,5,6 are the same three tokens earning in a
mock vault (`YIELD_TOKEN_<id>` / `YIELD_VAULT_<id>` / `YIELD_VENUE_<id>` in
`addresses.env`). The vaults start empty at a 1:1 share price; `MockERC4626`'s
`earn` / `lose` / `setLiquidityCap` move the index from a test or by hand.
Registration is permanent — `addYieldAsset` cannot re-point an id — so this
phase runs once per MASP, which is why `just redeploy` re-runs the whole chain
of scripts against a fresh one.

## Circuits

The relayer needs `tree_update_batch` artifacts, fetched from the
`lelantos-org/circuits` GitHub release into `circuits/` and mounted read-only at
`/circuits`. `just up` fetches them automatically; `just fetch-circuits` runs it
by hand and `just refetch-circuits` forces a re-download. The tag is pinned by
`CIRCUITS_VERSION` and cached via `circuits/.version`.

## Debugging

```sh
just env                 # resolved PROFILE / STACK_ENV / active config dir
just check               # validate compose (both envs), scripts, and every TOML
just addresses           # dump addresses.env written by the deploy one-shot
just deploy-logs         # contract addresses, funding, and errors from `deploy`
just show-config <svc>   # the TOML a running service actually mounted
just db-shell            # psql into Postgres
just sh <svc>            # shell into a container
DEBUG=1 just up          # verbose script output (TRACE=1 also enables `set -x`)
```

When a service behaves as though a contract address were missing, check
`just addresses` first, then confirm the chain id there matches a `[[chains]]`
block in the mounted TOML.

The deploy container keeps its full forge output at `/tmp/forge.log` and the
parsed address table at `/tmp/addresses.parsed`. `scripts/deploy-contracts.sh`
takes `CONTRACTS_DIR`, `WORK_DIR`, `OUT_FILE`, and `CHAIN_ID` from the
environment, so it can also be run outside the container against any RPC.

## Notes

- `just down` removes the Postgres and `addresses` volumes; `just stop` keeps them.
- Ports are published on all interfaces, and Postgres uses `postgres/postgres`.
  Both are fine for local use and neither is suitable for an exposed host.
- Rust images build from `backend/` via `backend/Dockerfile`, except the relayer,
  which builds from the repo root via `backend/crates/relayer/Dockerfile`.
- `relayer` must run as a single replica per chain — its tree mirror, nullifier
  guard, and idempotency cache are all per-process state. The compose stack runs
  one of each anyway; it matters when porting this to an orchestrator.
