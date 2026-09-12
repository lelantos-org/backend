# prices

USD spot prices for ERC20 tokens, shared by registry-webserver (`/v1/prices`)
and explorer-webserver (assets, flows, locked). A leaf library over `shared`: it
imports no other internal crate, since a price is keyed by chain id and token
address rather than by a stored row.

## Modules

| Module | What it owns |
|--------|--------------|
| `token` | `TokenKey` (`shared::chain::ChainId` + normalised hex address) and `TokenPrice` |
| `provider` | `PriceProvider` — the price-source interface |
| `providers` | one module per upstream; `defillama` today |
| `service` | `PriceService`: the shared cache in front of an ordered provider list |
| `convert` | `to_usd(base_units, asset_decimals, price)` |

## Adding a provider

1. New module under `providers/`, implementing `PriceProvider`: `name`,
   `supports_chain`, `fetch`.
2. Add it to the vector each binary hands `PriceService::new`.

Nothing else changes: consumers hold a `PriceService` and never name a provider
past the line that constructs one.

```rust
let service = PriceService::new(vec![Arc::new(DefiLlama::new(&base_url, timeout)?)], ttl);
let priced = service.for_tokens(&keys).await;
```

`ttl` is how long one token's answer is served before upstream is asked again;
it is floored at one second, since a zero would expire every entry on write and
turn the cache into a per-request round trip. The cache itself — its capacity and
its key type — is the service's business and is not part of the surface.

An empty provider list is legal and logs a warning at construction: the service
still answers, it just answers "unpriced" for everything, which is otherwise
indistinguishable from an outage in the endpoint's output.

## Resolution order

Providers are consulted in order — a fallback chain, not a race. A token reaches
the next provider only when the ones before it do not cover its chain, do not
know the token, or failed. `supports_chain` is checked first, so a chain nobody
covers (local anvil) costs no request at all.

## What is cached, and what is not

`PriceService` caches `Option<TokenPrice>` per token:

| Outcome | Cached | Why |
|---------|--------|-----|
| Priced | `Some(price)` | the answer, until the TTL |
| No provider knows it, or none covers its chain | `None` | a real answer; asked again once per TTL rather than once per request |
| Every provider that could speak for it errored | *nothing* | a transient outage must retry on the next request, not serve "unpriced" for a whole TTL |

`for_tokens` never fails. Prices decorate data that is useful without them, so a
dead upstream leaves the USD fields absent rather than failing the endpoint.

A cache hit is served, never rewritten: moka expires by write time, so writing
hits back would push a polled token's expiry out indefinitely and it would never
refresh. Repeated keys in one call are collapsed before any provider is asked —
several asset rows share one ERC-20 when a yield asset shadows a plain one.

`TokenKey::new` lowercases the address and strips a `0x` prefix, so the same
token written the wire's way and the `hex::encode` way is one cache entry and one
coin in one upstream request.

## Decimals

`to_usd` prefers the asset's own `decimals()` as the indexer read it over the
provider's, which reports decimals as metadata about a price feed; a
disagreement puts the dollar figure off by a power of ten. Values outside
`0..=38` are refused rather than converted, since a corrupt one would divide a
real amount down to `$0.00` and read as a measurement.

## Layering

May import: `shared`. Must NOT import `database`, any binary, or any service
crate. See [ARCHITECTURE.md](../../ARCHITECTURE.md).
