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
use alloy::primitives::{Address, U256};
use chain_types::decode::DepositNote;
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
        fee_bps_at_submit: u16,
        note: DepositNote,
        fee: DepositNote,
        pulled: U256,
    ) {
        self.escrowed.push(NewDepositEscrowed {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            deposit_id: u256_to_bigdecimal(id),
            payer: payer.as_slice().to_vec(),
            recipient: recipient.as_slice().to_vec(),
            public_asset_id: note.asset_id as i64,
            public_in: u256_to_bigdecimal(U256::from(note.value)),
            fee_bps_at_submit: i32::from(fee_bps_at_submit),
            inner: note.inner.0.to_vec(),
            aux: encode_aux(&note),
            fee_asset_id: fee.asset_id as i64,
            fee_in: u256_to_bigdecimal(U256::from(fee.value)),
            fee_inner: fee.inner.0.to_vec(),
            fee_aux: encode_aux(&fee),
            pulled: u256_to_bigdecimal(pulled),
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

/// One deposit note's aux blob as JSON, for the `aux` and `fee_aux` columns.
fn encode_aux(note: &DepositNote) -> serde_json::Value {
    json!({
        "clueRx": note.clue_rx.to_string(),
        "clueRy": note.clue_ry.to_string(),
        "ephPubX": note.eph_pub_x.to_string(),
        "ephPubY": note.eph_pub_y.to_string(),
        "ciphertext": format!("0x{}", hex::encode(&note.ciphertext)),
    })
}
