//! The dry run's verifier stub: every other on-chain check runs against dummy
//! proofs while the real ones are made.

use crate::adapters::rpc::RpcEndpoint;
use alloy::primitives::{Address, Bytes};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::rpc::types::state::{AccountOverride, StateOverride};

/// Runtime code that returns `true` for any call:
/// `PUSH1 1, PUSH1 0, MSTORE, PUSH1 32, PUSH1 0, RETURN`.
///
/// Installed over both verifiers in a dry run, so the pool's other checks run
/// against dummy proofs.
const ACCEPT_ALL: [u8; 10] = [0x60, 0x01, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];

/// Probe whether the chain's RPC honours `eth_call` code overrides, which a dry run
/// needs. Calls a codeless address with `ACCEPT_ALL` installed: a node that
/// applies the override answers `true`, one that ignores it answers nothing.
pub async fn supports_code_overrides(rpc: &RpcEndpoint) -> bool {
    let probe = Address::repeat_byte(0xde);
    let provider = ProviderBuilder::new().on_client(rpc.client());
    let tx = TransactionRequest::default().to(probe);
    match provider.call(&tx).overrides(&accept_all_at([probe])).await {
        Ok(out) => out.len() == 32 && out[31] == 1,
        Err(_) => false,
    }
}

/// State overrides installing [`ACCEPT_ALL`] as the code of every address.
pub(super) fn accept_all_at(addresses: impl IntoIterator<Item = Address>) -> StateOverride {
    let mut overrides = StateOverride::default();
    for address in addresses {
        overrides.insert(
            address,
            AccountOverride {
                code: Some(Bytes::from_static(&ACCEPT_ALL)),
                ..Default::default()
            },
        );
    }
    overrides
}
