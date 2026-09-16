//! The deposit ledger: `DepositEscrowed`, `DepositFlushed` and
//! `DepositCanceled` into `deposit_escrowed_events`, which is what the relayer
//! flushes from.
//!
//! Pure up to [`DepositPlan::apply`] — the push methods build rows and touch no
//! database.

use crate::domain::error::ProtocolIndexerError;
use crate::repositories::deposit_escrowed_events::{
    self, MarkCanceled, MarkFlushed, NewDepositEscrowed,
};
use alloy::primitives::{Address, B256, U256};
use chain_types::decode::DepositFeeNote;
use chain_types::numeric::u256_to_bigdecimal;
use database::{DbPool, RawEventRow};
use serde_json::json;

/// One window's writes to `deposit_escrowed_events`.
#[derive(Debug, Default)]
pub struct DepositPlan {
    pub escrowed: Vec<NewDepositEscrowed>,
    pub flushed: Vec<MarkFlushed>,
    pub canceled: Vec<MarkCanceled>,
}

impl DepositPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn push_escrowed(
        &mut self,
        chain_id: i64,
        row: &RawEventRow,
        id: U256,
        payer: Address,
        recipient: Address,
        public_asset_id: u64,
        public_in: u64,
        fee_bps_at_submit: u16,
        cm: B256,
        cv_dep_x: U256,
        cv_dep_y: U256,
        rcv: U256,
        aux: serde_json::Value,
        fee: DepositFeeNote,
    ) {
        self.escrowed.push(NewDepositEscrowed {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            deposit_id: u256_to_bigdecimal(id),
            payer: payer.as_slice().to_vec(),
            recipient: recipient.as_slice().to_vec(),
            public_asset_id: public_asset_id as i64,
            public_in: u256_to_bigdecimal(U256::from(public_in)),
            fee_bps_at_submit: i32::from(fee_bps_at_submit),
            cm: cm.0.to_vec(),
            cv_dep_x: u256_to_bigdecimal(cv_dep_x),
            cv_dep_y: u256_to_bigdecimal(cv_dep_y),
            rcv: u256_to_bigdecimal(rcv),
            aux,
            fee_asset_id: fee.fee_asset_id as i64,
            fee_in: u256_to_bigdecimal(U256::from(fee.fee_in)),
            fee_cm: fee.cm.0.to_vec(),
            fee_cv_dep_x: u256_to_bigdecimal(fee.cv_dep_x),
            fee_cv_dep_y: u256_to_bigdecimal(fee.cv_dep_y),
            fee_rcv: u256_to_bigdecimal(fee.rcv),
            // Built here rather than by the caller so the fee leaf's payload
            // keeps the same shape as the depositor's.
            fee_aux: encode_aux(
                fee.clue_rx,
                fee.clue_ry,
                fee.eph_pub_x,
                fee.eph_pub_y,
                &fee.ciphertext,
            ),
            // The digest the contract stored hashes `uint32(block.number)`,
            // which on Arbitrum is the L1 height rather than `row.block_number`.
            // Rows ingested before `evm_block_number` existed fall back to
            // `block_number`: correct on every chain except Arbitrum, whose rows
            // need an explicit repair.
            submitted_at_block: row.evm_block_number.unwrap_or(row.block_number),
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        });
    }

    /// A flush, as the `UPDATE` that records it needs it.
    pub fn push_flushed(&mut self, chain_id: i64, row: &RawEventRow, id: U256) {
        self.flushed.push(MarkFlushed {
            chain_id,
            deposit_id: u256_to_bigdecimal(id),
            block_number: row.block_number,
            block_ts: row.block_ts,
            tx_hash: row.tx_hash.clone(),
            log_index: row.log_index,
        });
    }

    pub fn push_canceled(&mut self, chain_id: i64, row: &RawEventRow, id: U256) {
        self.canceled.push(MarkCanceled {
            chain_id,
            deposit_id: u256_to_bigdecimal(id),
            block_number: row.block_number,
        });
    }

    pub async fn apply(&self, pool: &DbPool) -> Result<(), ProtocolIndexerError> {
        // Escrow before flush and cancel, which are `UPDATE`s keyed on the
        // deposit id this insert creates.
        deposit_escrowed_events::insert_batch(pool, &self.escrowed).await?;
        deposit_escrowed_events::mark_flushed_batch(pool, &self.flushed).await?;
        deposit_escrowed_events::mark_canceled_batch(pool, &self.canceled).await?;
        Ok(())
    }
}

/// Encode the deposit leaf's aux blob as JSON for the `aux` column. A deposit
/// occupies one leaf, so this is a single object rather than an array.
pub fn encode_aux(
    clue_rx: U256,
    clue_ry: U256,
    eph_pub_x: U256,
    eph_pub_y: U256,
    ciphertext: &[u8],
) -> serde_json::Value {
    json!({
        "clueRx": clue_rx.to_string(),
        "clueRy": clue_ry.to_string(),
        "ephPubX": eph_pub_x.to_string(),
        "ephPubY": eph_pub_y.to_string(),
        "ciphertext": format!("0x{}", hex::encode(ciphertext)),
    })
}
