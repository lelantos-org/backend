//! Routing one decoded event into the plan.
//!
//! Pure and infallible: every arm builds a row and pushes it. What used to be a
//! per-event `await` on the database is now an append to a `Vec`, and the whole
//! window is written once by [`CommitPlan::apply`].
//!
//! One exhaustive match with no wildcard arm, so a new `DecodedEvent` variant
//! fails to compile until this crate says what to do with it. Each arm delegates
//! to the concern that owns the projection — [`super::assets`],
//! [`super::yields`], [`super::tree`], [`super::deposits`] — so this module is
//! the routing table and nothing else.

use super::deposits::encode_aux;
use super::plan::CommitPlan;
use chain_types::decode::DecodedEvent;
use database::RawEventRow;

pub fn plan_event(plan: &mut CommitPlan, chain_id: i64, row: &RawEventRow, event: DecodedEvent) {
    match event {
        DecodedEvent::AssetRegistered {
            asset_id,
            token,
            scale,
        } => plan
            .assets
            .push_registered(chain_id, asset_id, token, scale),
        DecodedEvent::AssetFeeSet {
            asset_id,
            deposit_bps,
            withdraw_bps,
        } => plan
            .assets
            .push_fee(chain_id, asset_id, deposit_bps, withdraw_bps),
        DecodedEvent::RootAdvanced {
            start_index,
            inserted,
            old_root,
            new_root,
        } => plan
            .tree
            .push_advance(chain_id, row, start_index, inserted, old_root, new_root),
        // Owned by other consumers. `kinds()` keeps them out of the fetch, so
        // these arms exist only for exhaustiveness.
        DecodedEvent::NoteCreated { .. } => {}
        DecodedEvent::NullifierConsumed { .. } => {}
        DecodedEvent::AssetMoved { .. } => {}
        DecodedEvent::PerfFeeAccrued { .. } => {}
        DecodedEvent::NormalizedFeeSwept { .. } => {}
        DecodedEvent::DepositEscrowed {
            id,
            payer,
            recipient,
            public_asset_id,
            public_in,
            fee_bps_at_submit,
            cm,
            cv_dep_x,
            cv_dep_y,
            rcv,
            clue_rx,
            clue_ry,
            eph_pub_x,
            eph_pub_y,
            ciphertext,
            fee,
        } => {
            let aux = encode_aux(clue_rx, clue_ry, eph_pub_x, eph_pub_y, &ciphertext);
            plan.deposits.push_escrowed(
                chain_id,
                row,
                id,
                payer,
                recipient,
                public_asset_id,
                public_in,
                fee_bps_at_submit,
                cm,
                cv_dep_x,
                cv_dep_y,
                rcv,
                aux,
                fee,
            );
        }
        DecodedEvent::DepositFlushed { id, .. } => plan.deposits.push_flushed(chain_id, row, id),
        DecodedEvent::DepositCanceled { id, .. } => plan.deposits.push_canceled(chain_id, row, id),
        DecodedEvent::YieldAssetAdded {
            asset_id,
            venue,
            buffer_bps,
            perf_bps,
        } => plan
            .yields
            .push_binding(chain_id, asset_id, venue, buffer_bps, perf_bps),
        DecodedEvent::YieldParamsSet {
            asset_id,
            buffer_bps,
            perf_bps,
        } => plan
            .yields
            .push_params(chain_id, asset_id, buffer_bps, perf_bps),
        DecodedEvent::HaltedSet { asset_id, halted } => {
            plan.yields.push_halted(chain_id, asset_id, halted)
        }
        // Both only move backing between the venue and the pool's own balance;
        // the poller picks the new split up on its next pass.
        DecodedEvent::Rebalanced { .. } => {}
        DecodedEvent::EmergencyUnwound { .. } => {}
    }
}
