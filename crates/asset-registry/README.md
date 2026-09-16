# asset-registry

The asset catalog: the `assets` ⋈ `asset_yield` join, the circuit-unit arithmetic that prices a row, and a cached read of it.

`protocol-indexer` writes both tables. Everything else reads them through here.

## Why a library

Several binaries need the same row shape, and `backend/ARCHITECTURE.md` forbids one binary importing another:

| Consumer | Uses it for | Migrated? |
|---|---|---|
| `relayer` | fee decoration (`circuitAmount` on a quote) and shielded-fee admission | yes |
| `protocol-webserver` | serving the catalog | yes |
| `explorer-webserver` | asset-backed analytics | yes |

Before this crate there were two independent `AssetRow` structs — one in the relayer, one in explorer-webserver — over the same columns with different field names and different subsets, so any new column had to be added twice. Both are gone: there is now one row shape, and a new column is added once.

explorer-webserver reads the whole catalog rather than one chain's, which is what `list_all` is for — it holds no chain list of its own, so composing `chain_state` with `list_for_chains` would cost a round trip to learn something the query does not need.

## Errors and the `http` feature

`Error` is deliberately crate-local rather than `shared::http::AppError`, because `relayer` defines its own error type and a shared HTTP error here would not be the one it maps to.

The two webservers that *do* use `shared::http::AppError` would each have written the same `match`, so the `http` feature supplies `From<Error>` for it instead. It is off by default: enabling it pulls `shared/webserver`, and `relayer` must not be made to link axum to read an asset row.

## Units

`Scale` and `Rate` are the whole reason the row is not just a struct of `BigDecimal`s.

- A **plain** asset's circuit unit is worth `scale` base units, forever.
- A **yield** asset's is worth `gross / supply`, which rises with the venue while the unit count does not.

Both directions are asymmetric and that asymmetry is load-bearing: multiplying up is exact, dividing down must round **up**, or a quote underpays and the payment is refused. `Rate::to_circuit_ceil` returns `U256::MAX` for units outstanding against no backing — no finite number of worthless units covers a cost, and falling back to `scale` there would quote cheaply against units the pool would pay nothing for.

`AssetRow::rate()` returns `None` for a yield asset the indexer has not polled yet. Callers **must not** substitute `scale`: it is not a conservative default but wrong by whatever the venue has earned, in the direction that quotes too many units and then credits too few.

## Errors

`Error` is crate-local rather than `shared::http::AppError`, because `relayer` is one of the two binaries that define their own error type. Each consumer writes one `From<asset_registry::Error>` impl.

`Error::Numeric` means a `NUMERIC` column would not read as the integer it should be. Those columns are written by the indexer, so that is a fault on our side of the boundary — consumers map it to their internal-error variant, never to a 4xx.

## Caching

`AssetRegistry` holds a 30 s per-chain TTL. The registry is append-mostly, so the TTL is really about picking up a *new* asset promptly rather than correcting a stale one — with one exception: a yield asset's index moves every block. At 5% APY that is on the order of 1e-7 per minute, far below the grace the relayer's fee check already applies. The one discontinuity is a venue loss, after which a cached row over-values a unit until it expires — a small relayer loss, never a charge to a user.

Built through `shared::cache::build`, behind `shared`'s `cache` feature. That feature was split out of `webserver` for this crate: wanting an 18-line cache builder should not mean linking axum, and the gate that keeps the indexers axum-free is sharper for having `cache` and `http` on separate switches.
