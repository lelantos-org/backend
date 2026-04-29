CREATE TABLE notes (
    id            BIGSERIAL PRIMARY KEY,
    chain_id      BIGINT  NOT NULL,
    block_number  BIGINT  NOT NULL,
    tx_hash       BYTEA   NOT NULL,
    log_index     INTEGER NOT NULL,
    cm            BYTEA   NOT NULL,
    clue_rx       NUMERIC(78, 0) NOT NULL,
    clue_ry       NUMERIC(78, 0) NOT NULL,
    eph_pub_x     NUMERIC(78, 0) NOT NULL,
    eph_pub_y     NUMERIC(78, 0) NOT NULL,
    clue_bits_u16 INTEGER NOT NULL,
    ciphertext    BYTEA   NOT NULL,
    UNIQUE (chain_id, cm)
);
CREATE INDEX notes_chain_block_idx ON notes (chain_id, block_number);
CREATE INDEX notes_chain_id_idx    ON notes (chain_id, id);

CREATE TABLE nullifiers (
    id           BIGSERIAL PRIMARY KEY,
    chain_id     BIGINT NOT NULL,
    block_number BIGINT NOT NULL,
    tx_hash      BYTEA  NOT NULL,
    nullifier    BYTEA  NOT NULL,
    UNIQUE (chain_id, nullifier)
);
CREATE INDEX nullifiers_chain_block_idx ON nullifiers (chain_id, block_number);

CREATE TABLE subscriptions (
    id            BIGSERIAL  PRIMARY KEY,
    detection_key BYTEA      NOT NULL UNIQUE,
    gamma         INTEGER    NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    active        BOOLEAN    NOT NULL DEFAULT TRUE
);

CREATE TABLE matches (
    subscription_id BIGINT  NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    note_id         BIGINT  NOT NULL REFERENCES notes(id)         ON DELETE CASCADE,
    chain_id        BIGINT  NOT NULL,
    matched_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (subscription_id, note_id)
);
CREATE INDEX matches_sub_idx   ON matches (subscription_id, note_id DESC);
CREATE INDEX matches_chain_idx ON matches (chain_id, note_id);
