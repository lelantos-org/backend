-- The measured rate estimate, stored rather than cached in the writer.
--
-- The estimate used to live in a `moka` cache inside the relayer process that
-- computed it, read back by the same process to serve `/chains`. That only works
-- while exactly one process both measures and answers. The measurement is moving
-- to `registry-webserver`, which is stateless and runs as N replicas behind one
-- address: the replica that holds the advisory lock and does the archive reads
-- is not the replica that answers any given request, so a process-local cache
-- would serve a rate from one and nothing from the others.
--
-- Written by whichever process currently owns the venue-APY worker, and read by
-- every service that publishes the catalog.
ALTER TABLE asset_yield
    -- Annualized, in basis points, net of the pool's performance fee and idle
    -- buffer. NULL until the first measurement lands, which on a new deployment
    -- is until either a window of `asset_yield_sample` history exists or an
    -- archive node answers the vault path.
    --
    -- INTEGER, not SMALLINT: the estimate is capped at 1,000,000 bps (10,000%)
    -- before it is discarded as garbage, and SMALLINT tops out at 32,767.
    -- Negative is a real outcome — a venue can lose — and is floored at -10,000
    -- bps, a total loss.
    ADD COLUMN apy_bps         INTEGER,
    -- Seconds actually spanned by the two readings the rate came from, not the
    -- window that was aimed for. Published alongside the figure: a rate without
    -- its window is not a claim anyone can check.
    ADD COLUMN apy_window_s    BIGINT,
    -- When the worker last computed the estimate, on the writer's clock.
    --
    -- Deliberately not `asset_yield.updated_at`, which the indexer rewrites on
    -- its own heartbeat and would therefore report a months-old rate as current.
    -- This is what readers age the figure against, taking over the job the
    -- cache's TTL used to do: an asset that stops being measured — an RPC that
    -- lost its archive state, a venue that went away — goes back to publishing
    -- no rate rather than serving a stale one indefinitely.
    ADD COLUMN apy_measured_at TIMESTAMPTZ;
