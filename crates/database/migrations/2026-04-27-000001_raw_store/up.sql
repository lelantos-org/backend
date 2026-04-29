CREATE TABLE raw_events (
    id            BIGSERIAL PRIMARY KEY,
    chain_id      BIGINT   NOT NULL,
    block_number  BIGINT   NOT NULL,
    block_hash    BYTEA    NOT NULL,
    block_ts      BIGINT   NOT NULL,
    tx_hash       BYTEA    NOT NULL,
    log_index     INTEGER  NOT NULL,
    event_kind    SMALLINT NOT NULL,
    topics        BYTEA[]  NOT NULL,
    data          BYTEA    NOT NULL,
    UNIQUE (chain_id, block_number, log_index)
);
CREATE INDEX raw_events_chain_kind_id_idx ON raw_events (chain_id, event_kind, id);
CREATE INDEX raw_events_chain_block_idx   ON raw_events (chain_id, block_number);
CREATE INDEX raw_events_chain_tx_idx      ON raw_events (chain_id, tx_hash);

CREATE TABLE chain_state (
    chain_id            BIGINT PRIMARY KEY,
    last_block          BIGINT NOT NULL,
    last_block_hash     BYTEA  NOT NULL,
    last_scanned_block  BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE consumer_cursors (
    name              TEXT       NOT NULL,
    chain_id          BIGINT     NOT NULL,
    last_event_id     BIGINT     NOT NULL DEFAULT 0,
    last_block_number BIGINT     NOT NULL DEFAULT 0,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (name, chain_id)
);
