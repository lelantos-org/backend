-- Yield-index state, for assets whose custody earns in a venue.
--
-- An asset is yield-bearing iff it has a row here. The binding is created by
-- `YieldAssetAdded` and the contract cannot undo it — `MASP.addYieldAsset` goes
-- through the add-only registry, so a re-registration reverts and there is no
-- unbinding event — which is why presence is the flag rather than a separate
-- `assets.is_yield` column that could disagree with it.
--
-- Two kinds of column, filled by two different mechanisms:
--
--   * `venue`, `buffer_bps`, `perf_bps`, `halted` come from logs and are exact.
--   * everything from `total_normalized` down is polled from `yieldState`, and
--     is NULL until the first poll lands.
--
-- Not retracted on reorg, matching `assets`: the polled half is overwritten
-- every tick and the event-sourced half is re-applied when the consumer cursor
-- rewinds, so both self-heal without a delete.
CREATE TABLE asset_yield (
    chain_id                BIGINT      NOT NULL,
    asset_id_u64            BIGINT      NOT NULL,
    venue                   BYTEA       NOT NULL,
    buffer_bps              SMALLINT    NOT NULL,
    perf_bps                SMALLINT    NOT NULL,
    halted                  BOOLEAN     NOT NULL DEFAULT FALSE,

    -- Units owed to note holders, and the treasury's unswept units. Their sum
    -- is the supply the conversion divides by.
    total_normalized        NUMERIC(78, 0),
    accrued_fee_normalized  NUMERIC(78, 0),
    -- Underlying held by the pool for this asset and not in the venue.
    idle                    NUMERIC(78, 0),
    -- Fee high-water mark, in RAY.
    last_idx                NUMERIC(78, 0),
    -- Venue position plus idle: the numerator of the conversion.
    --
    -- Stored alongside supply rather than only as `index_ray` because the
    -- contract converts by `n * gross / supply`, in which `scale` and `RAY`
    -- both cancel. Rebuilding an amount from a rounded index reintroduces a
    -- rounding step the contract does not take, and disagrees with it at the
    -- boundary. `index_ray` is for display and charts.
    gross                   NUMERIC(78, 0),
    index_ray               NUMERIC(78, 0),

    block_number            BIGINT,
    updated_at              TIMESTAMPTZ,

    PRIMARY KEY (chain_id, asset_id_u64)
);

-- What the protocol earned, and what it took out.
--
-- The only record of either. A performance-fee accrual mints normalized units
-- to the treasury and moves no tokens, so it emits no `AssetMoved` and cannot
-- be reconstructed from `asset_flows`; a sweep does move tokens, but as a
-- treasury transfer rather than a pool flow.
--
-- `kind` 1 = accrued (units minted, `amount` NULL because nothing moved),
--        2 = swept   (units burned and `amount` paid, differing by the index).
CREATE TABLE yield_fee_events (
    id            BIGSERIAL PRIMARY KEY,
    chain_id      BIGINT        NOT NULL,
    asset_id_u64  BIGINT        NOT NULL,
    block_number  BIGINT        NOT NULL,
    block_ts      BIGINT        NOT NULL,
    tx_hash       BYTEA         NOT NULL,
    log_index     INTEGER       NOT NULL,
    kind          SMALLINT      NOT NULL,
    units         NUMERIC(78, 0) NOT NULL,
    amount        NUMERIC(78, 0),

    -- Replay-safe: a consumer cursor rewind re-reads the same logs.
    UNIQUE (chain_id, tx_hash, log_index)
);

CREATE INDEX yield_fee_events_asset_idx
    ON yield_fee_events (chain_id, asset_id_u64, block_number);
