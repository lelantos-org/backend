use super::union::BRANCHES;
use super::*;

/// The trimmed union must carry every branch the untrimmed one does. A kind
/// present in one and absent from the other would make the feed and the
/// counts disagree about what exists.
#[test]
fn both_unions_cover_every_kind() {
    let (plain, top) = (classified(), classified_top());
    for kind in ["'withdraw'", "'deposit'", "'pending'", "'transfer'"] {
        assert!(plain.contains(kind), "classified() is missing {kind}");
        assert!(top.contains(kind), "classified_top() is missing {kind}");
    }
    assert_eq!(plain.matches("UNION ALL").count(), BRANCHES.len() - 1);
    assert_eq!(top.matches("UNION ALL").count(), BRANCHES.len() - 1);
}

/// Every branch is trimmed, not merely the first. This is the whole reason
/// the branches are an array: a hand-written union grows a fifth branch
/// without the tail.
#[test]
fn every_branch_is_trimmed_by_the_limit() {
    let top = classified_top();
    assert_eq!(top.matches("LIMIT $3").count(), BRANCHES.len());
    assert_eq!(top.matches("ORDER BY").count(), BRANCHES.len());
}

/// The aggregate reads every row in the window, so a per-branch limit there
/// would silently undercount.
#[test]
fn the_aggregate_union_is_untrimmed() {
    let plain = classified();
    assert!(!plain.contains("LIMIT"), "{plain}");
    assert!(!plain.contains("ORDER BY"), "{plain}");
}

/// Every branch must be bounded by the since-ts floor, or one kind would
/// scan all history while the others honour the window.
#[test]
fn every_branch_is_bounded_by_the_since_floor() {
    assert_eq!(classified().matches(">= $2").count(), BRANCHES.len());
}

#[test]
fn an_explicit_since_is_passed_through() {
    assert_eq!(since_or_default(Some(1_700_000_000)), 1_700_000_000);
}

/// `0` is how a caller asks for all history; it must not be mistaken for
/// absent and replaced by the default window.
#[test]
fn zero_asks_for_all_history() {
    assert_eq!(since_or_default(Some(0)), 0);
    assert_eq!(since_or_default(Some(-5)), 0);
}

#[test]
fn an_absent_since_defaults_to_the_window() {
    let now = chrono::Utc::now().timestamp();
    let got = since_or_default(None);
    assert!(
        got <= now - DEFAULT_WINDOW_SEC,
        "{got} is inside the window"
    );
    // Bounded on the other side too, so a wrong sign cannot pass.
    assert!(got > now - DEFAULT_WINDOW_SEC - 60, "{got} is too far back");
}
