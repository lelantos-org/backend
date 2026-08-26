# Price oracle stub (dev)

`CoinbaseOracle` fetches `{base_url}/prices/{BASE}-{QUOTE}/{spot|buy}` and reads
`data.amount`, so serving that tree from a static file server gives the relayer
a real HTTP oracle with no network egress and no moving prices.

The alternative is the default `https://api.coinbase.com/v2`, which makes a dev
stack depend on the internet and on the market at the moment it booted — and a
fee quote that moves between runs is not something you can reason about locally.

`price(native, quote)` is **quote units per 1 native**, so `ETH-USD/spot` is
"what one ETH is worth in USD". Every fee token quotes in USD, which is why this
is the only pair here — the relayer asks for one pair per accepted token.

## Why the price is absurd

A shielded fee is `gas × gasPrice × price × 10^tokenDec / 10^18` base units,
divided by the asset's scale for circuit units. At a realistic ETH/USD, a single
transfer on an 18-decimal asset at scale 1e10 costs tens of thousands of circuit
units — more than a dev wallet ever deposits, so everything fails on
affordability rather than on anything real.

The value here lands a fee at roughly 1-3 circuit units against typical dev
amounts: small enough to afford, large enough that "the fee was taken" is not an
assertion against zero. Mirrors `e2e/config/oracle`.
