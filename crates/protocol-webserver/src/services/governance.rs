//! The governor's proposals and votes, shaped for the wire.
//!
//! Read straight from the tables protocol-indexer keeps, uncached here: a vote
//! should show up within the route's short `Cache-Control`, and the queries are
//! index scans bounded by the page size.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{
    ProposalActionOut, ProposalDetailOut, ProposalSummaryOut, ProposalsPageOut, TalliesOut,
    VoteOut, VotesPageOut,
};
use crate::repositories::Position;
use crate::repositories::gov_proposals::{self, ProposalRow};
use crate::repositories::gov_votes::{self, TallyRow, VoteRow};
use alloy::primitives::Address;
use bigdecimal::BigDecimal;
use chain_types::numeric::bigdecimal_to_u256;
use std::collections::HashMap;
use std::str::FromStr;

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 100;
const TITLE_MAX_CHARS: usize = 200;
/// A uint256 has at most 78 decimal digits.
const MAX_ID_DIGITS: usize = 78;

/// Proposals on one chain, newest first.
pub async fn list_proposals(
    st: &AppState,
    chain_id: i64,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> AppResult<ProposalsPageOut> {
    ensure_chain(st, chain_id)?;
    let before = cursor.map(parse_cursor).transpose()?;
    let limit = page_limit(limit);
    // One extra row says whether a next page exists without a count query.
    let mut rows = gov_proposals::list(&st.pool, chain_id, before, limit + 1).await?;
    let next_cursor = next_cursor(&mut rows, limit, |r| (r.block_number, r.log_index));

    let ids: Vec<BigDecimal> = rows.iter().map(|r| r.proposal_id.clone()).collect();
    let tallies = group_tallies(gov_votes::tallies(&st.pool, chain_id, &ids).await?);
    let proposals = rows
        .iter()
        .map(|r| summary(r, tallies.get(&r.proposal_id.normalized())))
        .collect();
    Ok(ProposalsPageOut {
        proposals,
        next_cursor,
    })
}

/// One proposal with its description and actions.
pub async fn get_proposal(
    st: &AppState,
    chain_id: i64,
    proposal_id: &str,
) -> AppResult<ProposalDetailOut> {
    ensure_chain(st, chain_id)?;
    let id = parse_proposal_id(proposal_id)?;
    let row = find(st, chain_id, &id).await?;
    let tallies =
        group_tallies(gov_votes::tallies(&st.pool, chain_id, std::slice::from_ref(&id)).await?);
    Ok(ProposalDetailOut {
        summary: summary(&row, tallies.get(&row.proposal_id.normalized())),
        description: row.description.clone(),
        actions: actions(&row),
    })
}

/// Votes on one proposal, newest first.
pub async fn list_votes(
    st: &AppState,
    chain_id: i64,
    proposal_id: &str,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> AppResult<VotesPageOut> {
    ensure_chain(st, chain_id)?;
    let id = parse_proposal_id(proposal_id)?;
    let before = cursor.map(parse_cursor).transpose()?;
    let limit = page_limit(limit);
    // A proposal this deployment never indexed is a 404, not an empty list —
    // even when orphan votes on it exist, as they do for a governor older than
    // the ingester's start block.
    find(st, chain_id, &id).await?;
    let mut rows = gov_votes::list(&st.pool, chain_id, &id, before, limit + 1).await?;
    let next_cursor = next_cursor(&mut rows, limit, |r| (r.block_number, r.log_index));
    Ok(VotesPageOut {
        votes: rows.iter().map(vote).collect(),
        next_cursor,
    })
}

/// An unconfigured chain is a 404, matching the catalog routes.
fn ensure_chain(st: &AppState, chain_id: i64) -> AppResult<()> {
    if st.serves_chain(chain_id) {
        Ok(())
    } else {
        Err(AppError::NotFound(format!("chain {chain_id}")))
    }
}

async fn find(st: &AppState, chain_id: i64, id: &BigDecimal) -> AppResult<ProposalRow> {
    gov_proposals::get(&st.pool, chain_id, id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("proposal {id}")))
}

fn page_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Trim the look-ahead row and name the position the next page starts after.
fn next_cursor<T>(rows: &mut Vec<T>, limit: i64, pos: impl Fn(&T) -> Position) -> Option<String> {
    if rows.len() as i64 <= limit {
        return None;
    }
    rows.truncate(limit as usize);
    rows.last().map(|r| format_cursor(pos(r)))
}

/// Opaque to clients; `<block>-<logIndex>` of the last row served.
fn format_cursor((block, log): Position) -> String {
    format!("{block}-{log}")
}

fn parse_cursor(raw: &str) -> AppResult<Position> {
    let bad = || AppError::BadRequest(format!("invalid cursor: {raw}"));
    let (block, log) = raw.split_once('-').ok_or_else(bad)?;
    Ok((
        block.parse().map_err(|_| bad())?,
        log.parse().map_err(|_| bad())?,
    ))
}

/// A decimal uint256, as the routes and `ProposalCreated` both spell it.
fn parse_proposal_id(raw: &str) -> AppResult<BigDecimal> {
    if raw.is_empty() || raw.len() > MAX_ID_DIGITS || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::BadRequest(format!(
            "proposalId must be a decimal uint256: {raw}"
        )));
    }
    BigDecimal::from_str(raw)
        .map_err(|_| AppError::BadRequest(format!("proposalId must be a decimal uint256: {raw}")))
}

/// Tally rows keyed by proposal. Keyed by the normalized integer, since the
/// same value can come back from Postgres with a different scale.
fn group_tallies(rows: Vec<TallyRow>) -> HashMap<BigDecimal, (TalliesOut, i64)> {
    let mut out: HashMap<BigDecimal, (TalliesOut, i64)> = HashMap::new();
    for r in rows {
        let entry = out
            .entry(r.proposal_id.normalized())
            .or_insert_with(|| (zero_tallies(), 0));
        let weight = decimal(&r.weight);
        match r.support {
            0 => entry.0.against = weight,
            1 => entry.0.for_votes = weight,
            2 => entry.0.abstain = weight,
            // `GovernorCountingSimple` rejects any other value, so none is
            // indexed; counted in `voteCount` regardless rather than dropped.
            _ => {}
        }
        entry.1 += r.votes;
    }
    out
}

fn zero_tallies() -> TalliesOut {
    TalliesOut {
        for_votes: "0".into(),
        against: "0".into(),
        abstain: "0".into(),
    }
}

fn summary(r: &ProposalRow, tallies: Option<&(TalliesOut, i64)>) -> ProposalSummaryOut {
    let (tallies, vote_count) = tallies.cloned().unwrap_or_else(|| (zero_tallies(), 0));
    ProposalSummaryOut {
        proposal_id: decimal(&r.proposal_id),
        proposer: address(&r.proposer),
        title: title(&r.description),
        vote_start: r.vote_start,
        vote_end: r.vote_end,
        quorum_vote_deadline: r.quorum_vote_deadline,
        created_block: r.block_number,
        created_tx: hex_bytes(&r.tx_hash),
        queued_at_block: r.queued_at_block,
        eta: r.eta,
        executed_at_block: r.executed_at_block,
        canceled_at_block: r.canceled_at_block,
        tallies,
        vote_count,
    }
}

/// The parallel arrays zipped into one entry per call. The governor refuses a
/// proposal whose arrays disagree in length, so `zip` drops nothing real.
fn actions(r: &ProposalRow) -> Vec<ProposalActionOut> {
    r.targets
        .iter()
        .zip(&r.call_values)
        .zip(&r.signatures)
        .zip(&r.calldatas)
        .map(
            |(((target, value), signature), calldata)| ProposalActionOut {
                target: address(target),
                value: decimal(value),
                signature: signature.clone(),
                calldata: hex_bytes(calldata),
            },
        )
        .collect()
}

fn vote(r: &VoteRow) -> VoteOut {
    VoteOut {
        voter: address(&r.voter),
        support: r.support,
        weight: decimal(&r.weight),
        reason: r.reason.clone(),
        block_number: r.block_number,
        tx_hash: hex_bytes(&r.tx_hash),
    }
}

/// The description's first non-empty line, markdown heading marks and
/// surrounding whitespace stripped, cut to 200 characters.
fn title(description: &str) -> String {
    description
        .lines()
        .map(|l| l.trim_start().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .chars()
        .take(TITLE_MAX_CHARS)
        .collect()
}

/// A uint256 `NUMERIC` as plain decimal digits.
///
/// Through `U256` rather than `BigDecimal`'s `Display`, which renders an
/// integer carrying a non-zero scale in scientific notation.
fn decimal(v: &BigDecimal) -> String {
    bigdecimal_to_u256(v)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| v.to_string())
}

/// EIP-55, or plain hex for a column that is not 20 bytes (which the indexer
/// never writes).
fn address(bytes: &[u8]) -> String {
    Address::try_from(bytes)
        .map(|a| a.to_checksum(None))
        .unwrap_or_else(|_| hex_bytes(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_title_is_the_first_nonempty_line_without_heading_marks() {
        assert_eq!(title("# Burn fees\n\nbody"), "Burn fees");
        assert_eq!(title("\n\n  ##   Spaced  \nbody"), "Spaced");
        assert_eq!(title("plain first line\nsecond"), "plain first line");
        assert_eq!(
            title("#\n## \nReal"),
            "Real",
            "a bare heading mark is empty"
        );
        assert_eq!(title(""), "");
    }

    /// Counted in characters, so a multi-byte title is not cut mid-codepoint.
    #[test]
    fn test_title_is_capped_at_200_characters() {
        let long = "é".repeat(250);
        assert_eq!(title(&long).chars().count(), 200);
    }

    #[test]
    fn test_cursor_round_trips_and_rejects_garbage() {
        assert_eq!(parse_cursor(&format_cursor((123, 4))).unwrap(), (123, 4));
        for bad in ["", "12", "a-b", "1-2-3", "-1"] {
            assert!(parse_cursor(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn test_proposal_id_must_be_a_decimal_uint256() {
        assert_eq!(
            parse_proposal_id("42").unwrap(),
            BigDecimal::from_str("42").unwrap()
        );
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert!(parse_proposal_id(max).is_ok());
        for bad in ["", "0x2a", "-1", "1.5", "1e3", &"9".repeat(79)] {
            assert!(parse_proposal_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn test_limit_defaults_and_clamps() {
        assert_eq!(page_limit(None), 20);
        assert_eq!(page_limit(Some(0)), 1);
        assert_eq!(page_limit(Some(-5)), 1);
        assert_eq!(page_limit(Some(1_000)), 100);
    }

    #[test]
    fn test_next_cursor_only_when_a_look_ahead_row_came_back() {
        let mut rows = vec![(10i64, 1i32), (9, 0), (8, 3)];
        assert_eq!(next_cursor(&mut rows, 2, |r| *r), Some("9-0".into()));
        assert_eq!(rows.len(), 2, "the look-ahead row is not served");

        let mut rows = vec![(10i64, 1i32), (9, 0)];
        assert_eq!(next_cursor(&mut rows, 2, |r| *r), None);
    }

    /// A value past `f64` precision stays exact, and an integer with a scale
    /// does not come out as `1E+20`.
    #[test]
    fn test_decimal_renders_plain_digits() {
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(decimal(&BigDecimal::from_str(max).unwrap()), max);
        let scaled = BigDecimal::new(1.into(), -20);
        assert_eq!(decimal(&scaled), format!("1{}", "0".repeat(20)));
    }

    #[test]
    fn test_tallies_fill_every_side_and_count_votes() {
        let id = BigDecimal::from(7);
        let got = group_tallies(vec![
            TallyRow {
                proposal_id: id.clone(),
                support: 1,
                weight: BigDecimal::from(500),
                votes: 2,
            },
            TallyRow {
                proposal_id: id.clone(),
                support: 0,
                weight: BigDecimal::from(50),
                votes: 1,
            },
        ]);
        let (t, n) = got.get(&id).unwrap();
        assert_eq!(t.for_votes, "500");
        assert_eq!(t.against, "50");
        assert_eq!(t.abstain, "0", "a side with no votes is zero, not absent");
        assert_eq!(*n, 3);
    }

    #[test]
    fn test_tallies_serialize_under_for_against_abstain() {
        let json = serde_json::to_value(zero_tallies()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"for": "0", "against": "0", "abstain": "0"})
        );
    }

    #[test]
    fn test_addresses_are_checksummed() {
        let lower = hex::decode("5fbdb2315678afecb367f032d93f642f64180aa3").unwrap();
        assert_eq!(
            address(&lower),
            "0x5FbDB2315678afecb367f032d93F642f64180aa3"
        );
    }
}
