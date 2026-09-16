//! FMD detection: every note in a batch against every subscriber, grouped by
//! gamma and fanned out over rayon.

use super::subscribers::SubEntry;
use crate::domain::convert::{bigdec_to_fq, clue_bits_be};
use crate::domain::error::{FmdIndexerError, Result};
use crate::repositories::matches::NewMatch;
use crate::repositories::notes::NoteRow;
use ark_ed_on_bn254::{Fq, Fr};
use crypto::clue::{CircomPoint, usable_as_clue};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Notes `scan` could not use.
///
/// Counted rather than dropped: a clue point no detection key may be multiplied
/// by is a note no subscriber can match, and nothing else reports it. The field
/// keeps its name because dashboards read it; what it counts is every point
/// `usable_as_clue` refuses, which is off-curve plus the identity and the points
/// outside the prime-order subgroup.
#[derive(Default)]
pub(super) struct ScanStats {
    pub(super) off_curve_notes: usize,
}

impl ScanStats {
    pub(super) fn absorb(&mut self, other: Self) {
        self.off_curve_notes += other.off_curve_notes;
    }
}

pub(super) struct ScanOutcome {
    pub(super) hits: Vec<NewMatch>,
    pub(super) stats: ScanStats,
}

/// Partition a note batch by chain, consuming it so no row is cloned.
pub(super) fn group_by_chain(notes: Vec<NoteRow>) -> BTreeMap<i64, Vec<NoteRow>> {
    let mut by_chain: BTreeMap<i64, Vec<NoteRow>> = BTreeMap::new();
    for note in notes {
        by_chain.entry(note.chain_id).or_default().push(note);
    }
    by_chain
}

/// Cartesian product of note by subscription, evaluated in parallel through rayon
/// on a blocking task. `clueBits` is the first two big-endian bytes of the
/// ciphertext.
///
/// Takes keys already parsed: the caller holds them across ticks, and parsing is
/// per-subscriber work that does not depend on the notes being scanned.
pub(super) async fn scan(
    notes: &[NoteRow],
    subs: &Arc<[SubEntry]>,
    chain_id: i64,
) -> Result<ScanOutcome> {
    let mut stats = ScanStats::default();

    let note_inputs: Arc<[(i64, Fq, Fq, u16)]> = notes
        .iter()
        .filter_map(|n| {
            let rx = bigdec_to_fq(&n.clue_rx);
            let ry = bigdec_to_fq(&n.clue_ry);
            // Screened once per note rather than left to the primitive, which
            // checks per gamma group. `usable_as_clue` is the same bar
            // `test_clue_batch_parsed` applies, so this only moves the work.
            if !usable_as_clue(&CircomPoint::new(rx, ry)) {
                stats.off_curve_notes += 1;
                return None;
            }
            // `plan_commit` refuses to store a note whose ciphertext cannot carry
            // the prefix, so the fallback is unreachable for stored rows.
            let bits = clue_bits_be(&n.ciphertext).unwrap_or(0);
            Some((n.id, rx, ry, bits))
        })
        .collect::<Vec<_>>()
        .into();

    let sub_inputs = subs.clone();

    let hits = tokio::task::spawn_blocking(move || {
        let subs = &*sub_inputs;
        // Group by gamma so each group runs as a single batch per note: one
        // fixed-base table per (note, gamma) amortizes the scalar multiplications
        // across the whole subscriber set.
        let mut by_gamma: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (idx, (_, _, g)) in subs.iter().enumerate() {
            by_gamma.entry(*g).or_default().push(idx);
        }

        // The key slices handed to `test_clue_batch_parsed` depend only on the
        // subscriber set, so they are built once for the whole batch; per-note
        // construction would re-materialise a Vec as long as the subscriber list
        // for every note. Each group is (gamma, subscriber indices, keys).
        type GammaGroup<'a> = (usize, Vec<usize>, Vec<&'a [Fr]>);
        let groups: Vec<GammaGroup> = by_gamma
            .into_iter()
            .map(|(gamma, indices)| {
                let dks: Vec<&[Fr]> = indices.iter().map(|&i| subs[i].1.as_ref()).collect();
                (gamma, indices, dks)
            })
            .collect();

        let per_note = |(nid, rx, ry, bits): &(i64, Fq, Fq, u16)| -> Vec<NewMatch> {
            let mut hits: Vec<NewMatch> = Vec::new();
            for (gamma, indices, dks) in &groups {
                let res = crypto::filter::test_clue_batch_parsed(dks, *rx, *ry, *bits, *gamma);
                for (k, hit) in res.iter().enumerate() {
                    if *hit {
                        hits.push(NewMatch {
                            subscription_id: subs[indices[k]].0,
                            note_id: *nid,
                            chain_id,
                        });
                    }
                }
            }
            hits
        };

        #[cfg(feature = "parallel")]
        {
            note_inputs
                .par_iter()
                .flat_map_iter(|n| per_note(n).into_iter())
                .collect()
        }
        #[cfg(not(feature = "parallel"))]
        {
            note_inputs.iter().flat_map(per_note).collect()
        }
    })
    .await
    .map_err(|e| FmdIndexerError::Crypto(e.to_string()))?;

    Ok(ScanOutcome { hits, stats })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;

    fn note(id: i64, chain_id: i64) -> NoteRow {
        NoteRow {
            id,
            chain_id,
            block_number: 0,
            tx_hash: Vec::new(),
            log_index: 0,
            cm: Vec::new(),
            clue_rx: BigDecimal::from(0),
            clue_ry: BigDecimal::from(0),
            eph_pub_x: BigDecimal::from(0),
            eph_pub_y: BigDecimal::from(0),
            ciphertext: Vec::new(),
            leaf_index: 0,
            cv_dep_x: BigDecimal::from(0),
            cv_dep_y: BigDecimal::from(0),
        }
    }

    #[test]
    fn group_by_chain_partitions_a_straddling_batch() {
        // The backfill pointer is a global note id, so a batch can interleave
        // chains. Every note must land in exactly one group, order preserved.
        let batch = vec![note(1, 10), note(2, 20), note(3, 10), note(4, 30)];

        let grouped = group_by_chain(batch);

        assert_eq!(grouped.keys().copied().collect::<Vec<_>>(), [10, 20, 30]);
        assert_eq!(ids(&grouped[&10]), [1, 3]);
        assert_eq!(ids(&grouped[&20]), [2]);
        assert_eq!(ids(&grouped[&30]), [4]);
    }

    #[test]
    fn group_by_chain_handles_an_empty_batch() {
        assert!(group_by_chain(Vec::new()).is_empty());
    }

    fn ids(notes: &[NoteRow]) -> Vec<i64> {
        notes.iter().map(|n| n.id).collect()
    }
}
