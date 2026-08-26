use crate::schema::*;
use bigdecimal::BigDecimal;
use diesel::prelude::*;

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable, Identifiable)]
#[diesel(table_name = raw_events, primary_key(id))]
pub struct RawEventRow {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = raw_events)]
pub struct NewRawEvent<'a> {
    pub chain_id: i64,
    pub block_number: i64,
    pub block_hash: &'a [u8],
    pub block_ts: i64,
    pub tx_hash: &'a [u8],
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, AsChangeset)]
#[diesel(table_name = chain_state, primary_key(chain_id))]
pub struct ChainStateRow {
    pub chain_id: i64,
    pub last_block: i64,
    pub last_block_hash: Vec<u8>,
    pub last_scanned_block: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = consumer_cursors, primary_key(name, chain_id))]
pub struct ConsumerCursorRow {
    pub name: String,
    pub chain_id: i64,
    pub last_event_id: i64,
    pub last_block_number: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = notes, primary_key(id))]
pub struct NoteRow {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub cm: Vec<u8>,
    pub clue_rx: BigDecimal,
    pub clue_ry: BigDecimal,
    pub eph_pub_x: BigDecimal,
    pub eph_pub_y: BigDecimal,
    pub ciphertext: Vec<u8>,
    pub leaf_index: i64,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
}

/// One leaf's tree inputs: `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
///
/// A projection of `notes` rather than the whole row: the tree mirrors read
/// millions of these and need nothing else.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = notes)]
pub struct LeafInputsRow {
    pub leaf_index: i64,
    pub cm: Vec<u8>,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
}

/// `token_hash` is absent: the indexer never needs the client capability, and
/// every webserver lookup supplies the hash it searches for rather than reading
/// one back.
#[derive(Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = subscriptions, primary_key(id))]
pub struct SubscriptionRow {
    pub id: i64,
    pub detection_key: Vec<u8>,
    pub gamma: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub active: bool,
    pub backfilled_through_note_id: i64,
}

/// Hand-written so a stray `{:?}` cannot print `detection_key`. The field is
/// omitted rather than masked, and `finish_non_exhaustive` renders the trailing
/// `..`.
impl std::fmt::Debug for SubscriptionRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionRow")
            .field("id", &self.id)
            .field("gamma", &self.gamma)
            .field("created_at", &self.created_at)
            .field("active", &self.active)
            .field(
                "backfilled_through_note_id",
                &self.backfilled_through_note_id,
            )
            .finish_non_exhaustive()
    }
}

/// The table's full column set, so the same struct is both what the indexer
/// inserts and what the webserver reads back.
#[derive(Debug, Clone, Insertable, Queryable, Selectable)]
#[diesel(table_name = tree_advances)]
pub struct TreeAdvanceRow {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub start_index: i64,
    pub inserted: i32,
    pub old_root: Vec<u8>,
    pub new_root: Vec<u8>,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}
