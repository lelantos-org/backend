//! The deployment registry, resolved once at boot.
//!
//! No database and no chain reads: these are facts an operator declares about
//! the deployment, and every one is identical for every relayer serving the
//! chain.

use crate::app::config::ChainCfg;
use crate::domain::responses::{ChainOut, ChainsResponse};
use alloy::primitives::Address;
use anyhow::{Context, Result};

/// Resolve every configured chain into the body `/v1/chains` serves.
///
/// Built at boot rather than per request, for the reason the relayer builds its
/// own descriptors that way: the handler then cannot reach the rest of the
/// config, and a malformed address fails startup instead of a wallet's first
/// call.
pub fn build(chains: &[ChainCfg]) -> Result<ChainsResponse> {
    let mut chains: Vec<ChainOut> = chains.iter().map(to_out).collect::<Result<_>>()?;
    chains.sort_by_key(|c| c.chain_id);
    Ok(ChainsResponse { chains })
}

/// Owned by this function rather than assembled in the handler, so adding a
/// config field cannot leak into the response by accident.
fn to_out(c: &ChainCfg) -> Result<ChainOut> {
    Ok(ChainOut {
        chain_id: c.chain_id,
        chain_name: c.name.clone(),
        rpc_url: c.rpc_url.clone(),
        read_rpc_url: c.read_rpc_url.clone(),
        explorer_url: c.explorer_url.clone(),
        permit2_address: checksummed(c.chain_id, "permit2_address", c.permit2_address.as_deref())?,
        masp_address: checksummed(c.chain_id, "masp_address", c.masp_address.as_deref())?,
        tree_depth: c.tree_depth,
        native_adapter_address: checksummed(
            c.chain_id,
            "native_adapter_address",
            c.native_adapter_address.as_deref(),
        )?,
        swap_wrapper_address: checksummed(
            c.chain_id,
            "swap_wrapper_address",
            c.swap_wrapper_address.as_deref(),
        )?,
        governor_address: nonzero(checksummed(
            c.chain_id,
            "governor_address",
            c.governor_address.as_deref(),
        )?),
        gov_token_address: nonzero(checksummed(
            c.chain_id,
            "gov_token_address",
            c.gov_token_address.as_deref(),
        )?),
        timelock_address: nonzero(checksummed(
            c.chain_id,
            "timelock_address",
            c.timelock_address.as_deref(),
        )?),
    })
}

/// Drop the zero address.
///
/// Governance is optional per chain, and the dev TOML declares these keys as
/// zero so the env overlay has something to rewrite. Publishing zero would
/// offer a wallet a governor that does not exist; absent tells it there is
/// none.
fn nonzero(addr: Option<String>) -> Option<String> {
    addr.filter(|a| a.parse::<Address>().is_ok_and(|a| !a.is_zero()))
}

/// Parse an address and re-emit it EIP-55 checksummed.
///
/// The canonicalisation is the point, not decoration. A relayer publishes the
/// pool it writes to as `Address::to_checksum`, and a wallet decides whether to
/// trust that relayer by comparing it against the `maspAddress` published here.
/// Passing the TOML string through verbatim would put a lowercase literal on one
/// side of that comparison and a checksummed one on the other, and every correct
/// relayer would be rejected.
fn checksummed(chain_id: i64, field: &str, raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let addr: Address = raw
        .parse()
        .with_context(|| format!("chain {chain_id}: {field} is not an address: {raw}"))?;
    Ok(Some(addr.to_checksum(None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(chain_id: i64, masp: Option<&str>) -> ChainCfg {
        ChainCfg {
            chain_id,
            name: Some("anvil".into()),
            rpc_url: Some("http://localhost:8545".into()),
            read_rpc_url: None,
            explorer_url: None,
            permit2_address: None,
            masp_address: masp.map(str::to_string),
            tree_depth: Some(11),
            native_adapter_address: None,
            swap_wrapper_address: None,
            governor_address: None,
            gov_token_address: None,
            timelock_address: None,
            apy_rpc_url: None,
        }
    }

    /// Published checksummed like every other address, and a zero placeholder
    /// is absent rather than a governor at `0x0`.
    #[test]
    fn test_governance_addresses_are_checksummed_and_zero_is_absent() {
        let mut c = cfg(31337, None);
        c.governor_address = Some("0x5fbdb2315678afecb367f032d93f642f64180aa3".into());
        c.gov_token_address = Some("0x0000000000000000000000000000000000000000".into());
        c.timelock_address = None;

        let out = to_out(&c).unwrap();
        assert_eq!(
            out.governor_address.as_deref(),
            Some("0x5FbDB2315678afecb367f032d93F642f64180aa3")
        );
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(
            json["governorAddress"],
            "0x5FbDB2315678afecb367f032d93F642f64180aa3"
        );
        assert!(
            json.get("govTokenAddress").is_none(),
            "zero is absent: {json}"
        );
        assert!(json.get("timelockAddress").is_none(), "{json}");
    }

    #[test]
    fn test_malformed_governor_is_rejected_at_build_time() {
        let mut c = cfg(31337, None);
        c.governor_address = Some("0xnope".into());
        let msg = format!("{:#}", build(&[c]).expect_err("must reject"));
        assert!(msg.contains("governor_address"), "{msg}");
    }

    /// The comparison a wallet makes is string equality against what a relayer
    /// publishes, and the relayer publishes `Address::to_checksum`. A lowercase
    /// literal here would fail that check against a perfectly good relayer.
    /// The two RPC fields serve different audiences and must stay independent.
    ///
    /// `rpcUrl` is installed into the user's wallet by
    /// `wallet_addEthereumChain`; `readRpcUrl` is the read-only proxy the SDK
    /// calls. Publishing the proxy in `rpcUrl` would make it the wallet's
    /// endpoint for that chain — permanently, per user — where it would be
    /// asked for writes and subscriptions it does not serve.
    #[test]
    fn test_the_read_rpc_url_is_published_without_replacing_the_wallet_one() {
        let mut c = cfg(1, None);
        c.read_rpc_url = Some("https://app.example.com/rpc/v1/1".into());

        let out = to_out(&c).unwrap();
        assert_eq!(out.rpc_url.as_deref(), Some("http://localhost:8545"));
        assert_eq!(
            out.read_rpc_url.as_deref(),
            Some("https://app.example.com/rpc/v1/1")
        );

        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["readRpcUrl"], "https://app.example.com/rpc/v1/1");
        assert_eq!(json["rpcUrl"], "http://localhost:8545");
    }

    /// A deployment without the proxy omits the field entirely, so a client
    /// falls back to `rpcUrl` rather than seeing an explicit null.
    #[test]
    fn test_an_unset_read_rpc_url_is_absent_not_null() {
        let json = serde_json::to_value(to_out(&cfg(1, None)).unwrap()).unwrap();
        assert!(json.get("readRpcUrl").is_none(), "absent, not null: {json}");
    }

    #[test]
    fn test_address_lowercase_in_config_is_published_checksummed() {
        let lower = "0x5fbdb2315678afecb367f032d93f642f64180aa3";
        let out = build(&[cfg(31337, Some(lower))]).expect("valid config");
        assert_eq!(
            out.chains[0].masp_address.as_deref(),
            Some("0x5FbDB2315678afecb367f032d93F642f64180aa3")
        );
    }

    /// Already-checksummed input must come out unchanged, so an operator who
    /// pastes the canonical form sees it preserved.
    #[test]
    fn test_address_already_checksummed_round_trips() {
        let mixed = "0x5FbDB2315678afecb367f032d93F642f64180aa3";
        let out = build(&[cfg(31337, Some(mixed))]).expect("valid config");
        assert_eq!(out.chains[0].masp_address.as_deref(), Some(mixed));
    }

    /// At boot, so it stops the service starting rather than surfacing as a
    /// mismatch in somebody's wallet.
    #[test]
    fn test_malformed_address_is_rejected_at_build_time() {
        let err = build(&[cfg(31337, Some("not-an-address"))]).expect_err("must reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("masp_address"), "{msg}");
        assert!(msg.contains("31337"), "names the chain: {msg}");
    }

    /// An undescribed field stays absent rather than becoming an empty string,
    /// so a client can fall back to its own default.
    #[test]
    fn test_undescribed_address_stays_absent() {
        let out = build(&[cfg(31337, None)]).expect("valid config");
        assert_eq!(out.chains[0].masp_address, None);
        assert_eq!(out.chains[0].permit2_address, None);
    }

    #[test]
    fn test_chains_are_sorted_by_id() {
        let out = build(&[cfg(31338, None), cfg(31337, None)]).expect("valid config");
        let ids: Vec<i64> = out.chains.iter().map(|c| c.chain_id).collect();
        assert_eq!(ids, vec![31337, 31338]);
    }
}
