//! The `eth_call` target allowlist: which contract, and which function on it.
//!
//! Bounds the reachable surface to the ~31 addresses and 11 functions the SDK
//! calls. Without it, anyone holding the URL has a general-purpose read node
//! billed to our upstream account.
//!
//! Selectors are grouped by address *class*, so adding a token extends a list
//! of addresses without touching a selector.
//!
//! Selectors are derived from signature strings at startup rather than written
//! as hex constants: a mistyped constant would refuse a function the SDK calls,
//! with nothing in the source to show why. The signatures correspond to
//! `sdk/src/chain/viem/abi.ts`.
//!
//! The set is static and changes only on redeploy, so this crate has no runtime
//! dependency on the registry. The cost is that an asset registered on-chain is
//! unreachable until the next converge; [`Rejection`] describes how that
//! surfaces.

use alloy::primitives::{Address, keccak256};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// The first four bytes of a call's `data`, identifying the function.
pub type Selector = [u8; 4];

/// What kind of contract an allowed address is.
///
/// Also the metric label for a rejection, so it must stay a closed set of
/// `&'static str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetClass {
    Masp,
    Permit2,
    Erc20,
    Venue,
    /// `LelantosGovernor`, read by the webapp's governance UI.
    Governor,
    /// The governance token: an ERC-20 with `ERC20Votes`.
    GovToken,
}

impl TargetClass {
    pub fn label(self) -> &'static str {
        match self {
            TargetClass::Masp => "masp",
            TargetClass::Permit2 => "permit2",
            TargetClass::Erc20 => "erc20",
            TargetClass::Venue => "venue",
            TargetClass::Governor => "governor",
            TargetClass::GovToken => "gov_token",
        }
    }

    /// The functions callable on this class.
    ///
    /// Mirrors `sdk/src/chain/viem/abi.ts`. `NATIVE_ADAPTER_ABI` has no entry
    /// here on purpose: the SDK uses it only to `encodeFunctionData` for writes,
    /// and writes never traverse this proxy, so the adapter is not an `eth_call`
    /// target at all.
    fn signatures(self) -> &'static [&'static str] {
        match self {
            // MASP_ABI reads, from `chain/viem/reads.ts`.
            TargetClass::Masp => &[
                "asset(uint64)",
                "yieldState(uint64)",
                "escrowed(uint256)",
                "isKnownRoot(bytes32)",
                "cancelDelay()",
            ],
            // PERMIT2_VIEW_ABI, from `chain/viem/permit2.ts`. Three arguments,
            // unlike the ERC-20 `allowance` below — a different function that
            // happens to share a name.
            TargetClass::Permit2 => &["allowance(address,address,address)"],
            // ERC20_ABI, from `chain/viem/token.ts`.
            TargetClass::Erc20 => &[
                "symbol()",
                "decimals()",
                "balanceOf(address)",
                "allowance(address,address)",
            ],
            // YIELD_VENUE_ABI. Reached only through `fetchAssetYield`.
            TargetClass::Venue => &["totalAssets()"],
            // `LelantosGovernor` views the webapp reads: OpenZeppelin Governor
            // v5 with GovernorSettings, CountingSimple, VotesQuorumFraction and
            // TimelockControl, plus the For/Abstain quorum-vote window. The two
            // writes at the top are here only as `eth_call` simulations: the
            // webapp runs each vote and proposal through one first so a custom-error
            // refusal (`QuorumVotingClosed`, `GovernorAlreadyCastVote`) can be
            // explained before the wallet prompts. An `eth_call` changes no
            // state; the transaction itself goes through the user's wallet.
            TargetClass::Governor => &[
                "castVoteWithReason(uint256,uint8,string)",
                "propose(address[],uint256[],bytes[],string)",
                "name()",
                "version()",
                "clock()",
                "CLOCK_MODE()",
                "COUNTING_MODE()",
                "token()",
                "timelock()",
                "state(uint256)",
                "proposalSnapshot(uint256)",
                "proposalDeadline(uint256)",
                "proposalProposer(uint256)",
                "proposalEta(uint256)",
                "proposalNeedsQueuing(uint256)",
                "proposalVotes(uint256)",
                "hasVoted(uint256,address)",
                "hashProposal(address[],uint256[],bytes[],bytes32)",
                "proposalThreshold()",
                "votingDelay()",
                "votingPeriod()",
                "quorum(uint256)",
                "quorumNumerator()",
                "getVotes(address,uint256)",
                "nonces(address)",
                "quorumVoteCutoff()",
                "proposalQuorumVoteDeadline(uint256)",
            ],
            // ERC20_ABI's reads plus `ERC20Votes`: the balance, the delegate and
            // the voting power the UI explains before a user votes. `delegate`
            // is an `eth_call` simulation target only, as with the governor's
            // writes above.
            TargetClass::GovToken => &[
                "delegate(address)",
                "symbol()",
                "decimals()",
                "balanceOf(address)",
                "allowance(address,address)",
                "name()",
                "totalSupply()",
                "delegates(address)",
                "getVotes(address)",
                "getPastVotes(address,uint256)",
                "getPastTotalSupply(uint256)",
                "clock()",
                "CLOCK_MODE()",
                "nonces(address)",
            ],
        }
    }
}

/// Why a call was refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Rejection {
    /// `data` shorter than a selector, or `to` missing or malformed.
    Malformed(&'static str),
    /// The contract is not in this chain's allowlist.
    ///
    /// The likeliest real cause is an asset registered on-chain without an
    /// `rpc-proxy` converge, which is why the handler logs the address at WARN
    /// and counts it: with no runtime refresh, that log is the only signal the
    /// allowlist has gone stale.
    UnknownAddress(Address),
    /// A known contract, called through a function it does not expose here.
    SelectorNotAllowed {
        class: TargetClass,
        selector: Selector,
    },
}

impl Rejection {
    /// Metric label. Bounded: five values, none derived from caller input.
    pub fn class_label(&self) -> &'static str {
        match self {
            Rejection::Malformed(_) => "malformed",
            Rejection::UnknownAddress(_) => "unknown",
            Rejection::SelectorNotAllowed { class, .. } => class.label(),
        }
    }

    /// What the caller is told.
    ///
    /// Names the address and selector. Neither is a secret — the caller supplied
    /// both — and naming them is what turns "the app mysteriously stopped
    /// showing a balance" into a one-line diagnosis from a browser console.
    pub fn message(&self) -> String {
        match self {
            Rejection::Malformed(why) => format!("eth_call: {why}"),
            Rejection::UnknownAddress(to) => {
                format!("eth_call: contract {to} is not served by this endpoint")
            }
            Rejection::SelectorNotAllowed { class, selector } => format!(
                "eth_call: function 0x{} is not served on this {} contract",
                hex::encode(selector),
                class.label()
            ),
        }
    }
}

/// One chain's allowed `(address, selector)` pairs.
#[derive(Debug, Clone)]
pub struct Targets {
    by_address: HashMap<Address, TargetClass>,
    by_class: HashMap<TargetClass, HashSet<Selector>>,
}

impl Targets {
    /// Build from the addresses in config.
    ///
    /// Later duplicates lose: an address listed both as an ERC-20 and as a venue
    /// keeps the first class it was given. That only arises from a generator bug,
    /// and the union of two selector sets would be a wider surface than either
    /// entry asked for.
    pub fn new(
        masp: Address,
        permit2: Address,
        erc20: impl IntoIterator<Item = Address>,
        venue: impl IntoIterator<Item = Address>,
    ) -> Self {
        let mut by_address = HashMap::new();
        by_address.insert(masp, TargetClass::Masp);
        by_address.entry(permit2).or_insert(TargetClass::Permit2);
        for a in erc20 {
            by_address.entry(a).or_insert(TargetClass::Erc20);
        }
        for a in venue {
            by_address.entry(a).or_insert(TargetClass::Venue);
        }

        let by_class = [
            TargetClass::Masp,
            TargetClass::Permit2,
            TargetClass::Erc20,
            TargetClass::Venue,
            TargetClass::Governor,
            TargetClass::GovToken,
        ]
        .into_iter()
        .map(|c| (c, c.signatures().iter().map(|s| selector(s)).collect()))
        .collect();

        Self {
            by_address,
            by_class,
        }
    }

    /// Add the governance contracts, each optional per chain.
    ///
    /// The governor keeps the "first class wins" rule, since sharing an address
    /// with the pool is a config bug. The token instead replaces an `Erc20`
    /// entry for the same address: LNT may also be a registered asset, its
    /// selector set is a superset of the ERC-20 one, and keeping the narrower
    /// class would refuse `delegates` and `getVotes` on it.
    pub fn with_governance(
        mut self,
        governor: Option<Address>,
        gov_token: Option<Address>,
    ) -> Self {
        if let Some(g) = governor {
            self.by_address.entry(g).or_insert(TargetClass::Governor);
        }
        if let Some(t) = gov_token {
            let class = self.by_address.entry(t).or_insert(TargetClass::GovToken);
            if *class == TargetClass::Erc20 {
                *class = TargetClass::GovToken;
            }
        }
        self
    }

    /// How many addresses are allowlisted, for the startup banner. An operator
    /// reading "3 addresses" where they expected 31 has found the misconfigured
    /// deploy before any user does.
    pub fn len(&self) -> usize {
        self.by_address.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_address.is_empty()
    }

    /// Whether `to` may be called through `selector`.
    ///
    /// A `None` selector — no `data`, or fewer than four bytes of it — is
    /// refused rather than treated as a call to the fallback function. The SDK
    /// calls no fallback, and permitting one would allow zero-length probes
    /// against every allowlisted address.
    pub fn check(
        &self,
        to: Option<Address>,
        selector: Option<Selector>,
    ) -> Result<TargetClass, Rejection> {
        let Some(to) = to else {
            return Err(Rejection::Malformed("missing `to`"));
        };
        let Some(selector) = selector else {
            return Err(Rejection::Malformed("calldata shorter than a selector"));
        };
        let class = *self
            .by_address
            .get(&to)
            .ok_or(Rejection::UnknownAddress(to))?;

        if self.by_class[&class].contains(&selector) {
            Ok(class)
        } else {
            Err(Rejection::SelectorNotAllowed { class, selector })
        }
    }
}

/// The contract and function an `eth_call`'s params name, as [`Targets::check`]
/// wants them.
///
/// One reader for both callers: the allowlist check that refuses a call and the
/// counter that classifies the refusal. Read apart, the two could disagree about
/// which pair was refused and the WARN would name a contract that was never
/// checked.
///
/// Either half is `None` when the params do not carry a usable one; `check`
/// turns that into a [`Rejection::Malformed`].
pub fn call_target(params: &[Value]) -> (Option<Address>, Option<Selector>) {
    let call = params.first().and_then(Value::as_object);
    let to = call
        .and_then(|c| c.get("to"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Address>().ok());
    // Both spellings name the same bytes; viem sends `data`, some tooling
    // sends `input`.
    let selector = call
        .and_then(|c| c.get("data").or_else(|| c.get("input")))
        .and_then(Value::as_str)
        .and_then(selector_of);
    (to, selector)
}

/// The selector at the front of hex-encoded calldata.
///
/// Reads the first four bytes without decoding the rest. Calldata is capped at
/// 8 KB and this runs on every `eth_call`, so decoding the whole field to look
/// at its first four bytes would allocate up to 8 KB per request to answer a
/// question the first eight characters settle.
///
/// `None` for anything that cannot carry a selector — too short, or not hex.
pub fn selector_of(data_hex: &str) -> Option<Selector> {
    let hex = data_hex
        .strip_prefix("0x")
        .or_else(|| data_hex.strip_prefix("0X"))
        .unwrap_or(data_hex);
    let head = hex.get(..8)?;
    let mut out = [0u8; 4];
    hex::decode_to_slice(head, &mut out).ok()?;
    Some(out)
}

/// The 4-byte selector for a function signature.
fn selector(signature: &str) -> Selector {
    keccak256(signature.as_bytes())[..4]
        .try_into()
        .expect("keccak256 is 32 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const MASP: Address = address!("1111111111111111111111111111111111111111");
    const PERMIT2: Address = address!("2222222222222222222222222222222222222222");
    const TOKEN: Address = address!("3333333333333333333333333333333333333333");
    const VENUE: Address = address!("4444444444444444444444444444444444444444");
    const STRANGER: Address = address!("9999999999999999999999999999999999999999");

    fn targets() -> Targets {
        Targets::new(MASP, PERMIT2, [TOKEN], [VENUE])
    }

    /// Hex calldata for `sig`, padded with a plausible argument so the length
    /// check is not what makes the case pass.
    fn call(sig: &str) -> Option<Selector> {
        let mut v = selector(sig).to_vec();
        v.extend_from_slice(&[0u8; 32]);
        selector_of(&format!("0x{}", hex::encode(v)))
    }

    /// The eleven functions the SDK actually issues. If this fails, a real read
    /// path is broken.
    #[test]
    fn every_selector_the_sdk_issues_is_accepted() {
        let t = targets();
        let expected = [
            (MASP, "asset(uint64)"),
            (MASP, "yieldState(uint64)"),
            (MASP, "escrowed(uint256)"),
            (MASP, "isKnownRoot(bytes32)"),
            (MASP, "cancelDelay()"),
            (PERMIT2, "allowance(address,address,address)"),
            (TOKEN, "symbol()"),
            (TOKEN, "decimals()"),
            (TOKEN, "balanceOf(address)"),
            (TOKEN, "allowance(address,address)"),
            (VENUE, "totalAssets()"),
        ];
        assert_eq!(expected.len(), 11, "the inventory is eleven selectors");

        for (to, sig) in expected {
            assert!(
                t.check(Some(to), call(sig)).is_ok(),
                "{sig} on {to} must be allowed"
            );
        }
    }

    /// Guards the derivation itself against a change in how selectors are
    /// computed. These four are widely published ERC-20 values, so they pin the
    /// keccak path to something independently checkable.
    #[test]
    fn selectors_match_the_published_erc20_values() {
        assert_eq!(hex::encode(selector("balanceOf(address)")), "70a08231");
        assert_eq!(hex::encode(selector("decimals()")), "313ce567");
        assert_eq!(hex::encode(selector("symbol()")), "95d89b41");
        assert_eq!(
            hex::encode(selector("allowance(address,address)")),
            "dd62ed3e"
        );
    }

    /// Class isolation. The two `allowance` functions differ only in arity, so
    /// this is the case where a per-address-only allowlist would let a Permit2
    /// call through to a token, or the reverse.
    #[test]
    fn a_selector_from_another_class_is_refused() {
        let t = targets();

        let err = t
            .check(Some(TOKEN), call("allowance(address,address,address)"))
            .unwrap_err();
        assert!(matches!(
            err,
            Rejection::SelectorNotAllowed {
                class: TargetClass::Erc20,
                ..
            }
        ));

        let err = t.check(Some(TOKEN), call("cancelDelay()")).unwrap_err();
        assert_eq!(err.class_label(), "erc20");

        let err = t.check(Some(MASP), call("balanceOf(address)")).unwrap_err();
        assert_eq!(err.class_label(), "masp");
    }

    /// The stale-allowlist case: a token registered on-chain without a converge.
    /// The rejection names the address, so the cause is identifiable.
    #[test]
    fn an_unlisted_address_is_refused_and_named() {
        let err = targets()
            .check(Some(STRANGER), call("balanceOf(address)"))
            .unwrap_err();

        assert_eq!(err, Rejection::UnknownAddress(STRANGER));
        assert_eq!(err.class_label(), "unknown");
        assert!(
            err.message().contains(&STRANGER.to_string()),
            "{}",
            err.message()
        );
    }

    /// A bare `to` with no calldata is a fallback-function probe. Nothing the
    /// SDK does looks like this, and allowing it would let an abuser sweep every
    /// allowlisted address.
    #[test]
    fn calldata_shorter_than_a_selector_is_refused() {
        let t = targets();
        for short in ["", "0x", "0x70a082"] {
            assert!(
                matches!(
                    t.check(Some(TOKEN), selector_of(short)).unwrap_err(),
                    Rejection::Malformed(_)
                ),
                "{short:?}"
            );
        }
    }

    /// The selector is read from the front of the hex without decoding the
    /// rest, so a large calldata costs no more than a small one.
    #[test]
    fn a_selector_is_read_without_decoding_the_whole_calldata() {
        let big = format!("0x70a08231{}", "ab".repeat(4096));
        assert_eq!(selector_of(&big), Some([0x70, 0xa0, 0x82, 0x31]));

        // With and without the prefix, and case-insensitively.
        assert_eq!(selector_of("0x70A08231"), Some([0x70, 0xa0, 0x82, 0x31]));
        assert_eq!(selector_of("70a08231"), Some([0x70, 0xa0, 0x82, 0x31]));

        // Not hex, and a multi-byte char that must not be sliced through.
        assert_eq!(selector_of("0xzzzzzzzz"), None);
        assert_eq!(selector_of("0x70a0é231"), None);
    }

    #[test]
    fn a_missing_to_is_refused() {
        assert!(matches!(
            targets().check(None, call("symbol()")).unwrap_err(),
            Rejection::Malformed(_)
        ));
    }

    const GOVERNOR: Address = address!("5555555555555555555555555555555555555555");
    const GOV_TOKEN: Address = address!("6666666666666666666666666666666666666666");

    /// What the governance UI reads, on the contract it reads it from.
    #[test]
    fn governance_reads_are_accepted_on_their_own_contracts() {
        let t = targets().with_governance(Some(GOVERNOR), Some(GOV_TOKEN));
        for sig in [
            "state(uint256)",
            "proposalVotes(uint256)",
            "hasVoted(uint256,address)",
            "getVotes(address,uint256)",
            "proposalQuorumVoteDeadline(uint256)",
            "quorum(uint256)",
            "clock()",
        ] {
            assert_eq!(
                t.check(Some(GOVERNOR), call(sig)),
                Ok(TargetClass::Governor),
                "{sig}"
            );
        }
        for sig in [
            "balanceOf(address)",
            "delegates(address)",
            "getVotes(address)",
        ] {
            assert_eq!(
                t.check(Some(GOV_TOKEN), call(sig)),
                Ok(TargetClass::GovToken),
                "{sig}"
            );
        }
        // Writes are not reads, and classes stay apart.
        assert!(
            t.check(Some(GOVERNOR), call("castVote(uint256,uint8)"))
                .is_err()
        );
        assert!(t.check(Some(GOVERNOR), call("balanceOf(address)")).is_err());
        assert!(t.check(Some(TOKEN), call("delegates(address)")).is_err());
    }

    /// Without governance configured, the contracts are unknown like any other.
    #[test]
    fn absent_governance_adds_nothing() {
        let t = targets().with_governance(None, None);
        assert_eq!(t.len(), 4);
        assert_eq!(
            t.check(Some(GOVERNOR), call("state(uint256)")),
            Err(Rejection::UnknownAddress(GOVERNOR))
        );
    }

    /// LNT may also be a registered asset; it must still expose `delegates`.
    #[test]
    fn the_gov_token_widens_a_matching_erc20_entry_but_not_the_pool() {
        let t = Targets::new(MASP, PERMIT2, [GOV_TOKEN], [])
            .with_governance(Some(MASP), Some(GOV_TOKEN));
        assert!(t.check(Some(GOV_TOKEN), call("delegates(address)")).is_ok());
        assert!(t.check(Some(GOV_TOKEN), call("symbol()")).is_ok());
        assert_eq!(
            t.check(Some(MASP), call("cancelDelay()")),
            Ok(TargetClass::Masp),
            "a governor configured at the pool's address does not replace it"
        );
    }

    /// A generator that emitted one address in two lists must not widen that
    /// address's surface to the union of both classes.
    #[test]
    fn a_duplicated_address_keeps_one_class() {
        let t = Targets::new(MASP, PERMIT2, [TOKEN], [TOKEN]);
        assert_eq!(t.len(), 3);
        assert!(t.check(Some(TOKEN), call("symbol()")).is_ok());
        assert!(t.check(Some(TOKEN), call("totalAssets()")).is_err());
    }
}
