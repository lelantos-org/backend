-- Address screening list backing `risk-webserver`. One row per
-- (chain, address, source); an address listed by several sources gets
-- several rows and screens as the *max* risk across them.
--
-- `address` is TEXT, not BYTEA, unlike every other address-shaped column in
-- this schema. Deliberate: the list must hold non-EVM address formats
-- (base58, bech32) without a migration, and `chain` is the address *family*
-- ('evm', 'btc', …), not a network id — sanctions apply to an address across
-- every chain in its family, so this table is not scoped by `chain_id`.
--
-- OPERATIONAL NOTE: rows store the NORMALIZED form produced by
-- `risk_webserver::domain::address::normalize` — `chain` lowercased, and for
-- `chain = 'evm'` the address lowercased 0x-hex. Lookups are exact `=`
-- matches, so a row inserted with a checksummed (mixed-case) EVM address will
-- never be found. There is no write API; whatever populates this table must
-- normalize first.
--
-- `risk` is constrained here rather than left to the application because the
-- table is populated out-of-band by SQL, where no application code runs.
-- `address` intentionally has no shape CHECK: the column is chain-agnostic.

CREATE TABLE screened_addresses (
    id       BIGSERIAL   PRIMARY KEY,
    chain    TEXT        NOT NULL,
    address  TEXT        NOT NULL,
    risk     TEXT        NOT NULL,
    source   TEXT        NOT NULL,
    reason   TEXT,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT screened_addresses_risk_check
        CHECK (risk IN ('banned', 'high', 'medium', 'low')),
    CONSTRAINT screened_addresses_chain_address_source_key
        UNIQUE (chain, address, source)
);

-- Screening lookup: `WHERE chain = $1 AND address = ANY($2)`.
CREATE INDEX screened_addresses_lookup_idx ON screened_addresses (chain, address);
