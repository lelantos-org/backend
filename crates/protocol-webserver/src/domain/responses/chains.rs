use serde::Serialize;
use utoipa::ToSchema;

/// One chain, as the deployment declares it.
///
/// Deliberately holds nothing about any particular relayer. A relayer publishes
/// what only it knows — its signer, its mirror state, its fee policy — on its own
/// `GET /chains`; this is the half that is identical for every relayer
/// serving the chain, so a self-hosted one need not be configured with it.
///
/// `maspAddress` and `treeDepth` appear in both places on purpose: a wallet
/// compares them and refuses a relayer pointed at a different pool, or mirroring
/// a different tree shape, rather than discovering it after building a proof.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainOut {
    pub chain_id: i64,
    /// Every field below is absent rather than null when the operator has not
    /// described it, so a client can fall back to its own default instead of
    /// treating "undescribed" as "empty".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_name: Option<String>,
    /// Browser-reachable RPC, not the endpoint this service reads with.
    ///
    /// What a wallet is offered as the chain's endpoint. See `readRpcUrl` for
    /// the SDK's own read traffic; the two are separate because this one is
    /// installed into the user's wallet permanently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    /// Read-only RPC for the SDK's `eth_call`/`eth_getLogs` traffic.
    ///
    /// Absent means "use `rpcUrl`", which is what a deployment without the read
    /// proxy does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_rpc_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explorer_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permit2_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub masp_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_depth: Option<u32>,
    /// `NativeAdapter`, when the deployment has one. Absent means native-coin
    /// deposit and withdraw have no entry point on this chain, and a wallet
    /// withholds the option rather than offering one that reverts at submit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_adapter_address: Option<String>,
    /// `SwapWrapper`, when the deployment has one. Absent disables swaps here.
    ///
    /// A property of the deployment rather than of a relayer: the wrapper is the
    /// contract a swap is routed through, and every relayer on the chain routes
    /// through the same one. A relayer that has not been configured with it
    /// simply declines the swap, which is its own answer and not this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_wrapper_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainsResponse {
    pub chains: Vec<ChainOut>,
}
