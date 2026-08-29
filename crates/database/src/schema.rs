// @generated. Re-run `diesel print-schema` after migration changes.

diesel::table! {
    raw_events (id) {
        id -> Int8,
        chain_id -> Int8,
        block_number -> Int8,
        evm_block_number -> Nullable<Int8>,
        block_hash -> Bytea,
        block_ts -> Int8,
        tx_hash -> Bytea,
        log_index -> Int4,
        event_kind -> Int2,
        topics -> Array<Bytea>,
        data -> Bytea,
    }
}

diesel::table! {
    chain_state (chain_id) {
        chain_id -> Int8,
        last_block -> Int8,
        last_block_hash -> Bytea,
        last_scanned_block -> Int8,
    }
}

diesel::table! {
    consumer_cursors (name, chain_id) {
        name -> Text,
        chain_id -> Int8,
        last_event_id -> Int8,
        last_block_number -> Int8,
        updated_at -> Timestamptz,
        last_reorg_id -> Int8,
    }
}

diesel::table! {
    chain_reorgs (id) {
        id -> Int8,
        chain_id -> Int8,
        rewind_to -> Int8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    notes (id) {
        id -> Int8,
        chain_id -> Int8,
        block_number -> Int8,
        tx_hash -> Bytea,
        log_index -> Int4,
        cm -> Bytea,
        clue_rx -> Numeric,
        clue_ry -> Numeric,
        eph_pub_x -> Numeric,
        eph_pub_y -> Numeric,
        ciphertext -> Bytea,
        leaf_index -> Int8,
        cv_dep_x -> Numeric,
        cv_dep_y -> Numeric,
    }
}

diesel::table! {
    subscriptions (id) {
        id -> Int8,
        detection_key -> Bytea,
        gamma -> Int4,
        created_at -> Timestamptz,
        active -> Bool,
        token_hash -> Nullable<Bytea>,
        backfilled_through_note_id -> Int8,
    }
}

diesel::table! {
    matches (subscription_id, note_id) {
        subscription_id -> Int8,
        note_id -> Int8,
        chain_id -> Int8,
        matched_at -> Timestamptz,
    }
}

diesel::table! {
    assets (chain_id, asset_id_u64) {
        chain_id -> Int8,
        asset_id_u64 -> Int8,
        token -> Bytea,
        scale -> Numeric,
        decimals -> Nullable<Int2>,
        symbol -> Nullable<Text>,
        deposit_bps -> Nullable<Int2>,
        withdraw_bps -> Nullable<Int2>,
    }
}

diesel::table! {
    tree_advances (chain_id, block_number, log_index) {
        chain_id -> Int8,
        block_number -> Int8,
        log_index -> Int4,
        start_index -> Int8,
        inserted -> Int4,
        old_root -> Bytea,
        new_root -> Bytea,
        tx_hash -> Bytea,
        block_ts -> Int8,
    }
}

diesel::table! {
    asset_flows (chain_id, block_number, log_index) {
        chain_id -> Int8,
        block_number -> Int8,
        log_index -> Int4,
        asset_id_u64 -> Int8,
        token -> Bytea,
        in_amount -> Numeric,
        out_amount -> Numeric,
        tx_hash -> Bytea,
        block_ts -> Int8,
        public_in -> Nullable<Numeric>,
        public_out -> Nullable<Numeric>,
    }
}

diesel::table! {
    spent_nullifiers (chain_id, block_number, log_index) {
        chain_id -> Int8,
        block_number -> Int8,
        log_index -> Int4,
        nf -> Bytea,
        tx_hash -> Bytea,
        block_ts -> Int8,
        seq -> Int8,
    }
}

diesel::table! {
    deposit_escrowed_events (chain_id, block_number, log_index) {
        chain_id -> Int8,
        block_number -> Int8,
        log_index -> Int4,
        deposit_id -> Numeric,
        payer -> Bytea,
        recipient -> Bytea,
        public_asset_id -> Int8,
        public_in -> Numeric,
        fee_bps_at_submit -> Int4,
        cm -> Bytea,
        cv_dep_x -> Numeric,
        cv_dep_y -> Numeric,
        rcv -> Numeric,
        aux -> Jsonb,
        fee_in -> Numeric,
        fee_cm -> Bytea,
        fee_cv_dep_x -> Numeric,
        fee_cv_dep_y -> Numeric,
        fee_rcv -> Numeric,
        fee_aux -> Jsonb,
        submitted_at_block -> Int8,
        flushed_at_block -> Nullable<Int8>,
        flushed_at_ts -> Nullable<Int8>,
        flushed_tx_hash -> Nullable<Bytea>,
        canceled_at_block -> Nullable<Int8>,
        tx_hash -> Bytea,
        block_ts -> Int8,
    }
}

diesel::table! {
    screened_addresses (id) {
        id -> Int8,
        chain -> Text,
        address -> Text,
        risk -> Text,
        source -> Text,
        reason -> Nullable<Text>,
        added_at -> Timestamptz,
    }
}

diesel::joinable!(matches -> subscriptions (subscription_id));
diesel::joinable!(matches -> notes (note_id));

diesel::allow_tables_to_appear_in_same_query!(
    raw_events,
    chain_state,
    consumer_cursors,
    chain_reorgs,
    notes,
    subscriptions,
    matches,
    assets,
    tree_advances,
    asset_flows,
    spent_nullifiers,
    deposit_escrowed_events,
    screened_addresses,
);
