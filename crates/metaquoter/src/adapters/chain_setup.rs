use crate::domain::fees::{WRAPPER_OVERHEAD_GAS, max_deposit};
use crate::domain::models::{Quote, QuoteRequest, Venue};
use crate::domain::time::now_secs;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::transports::http::{Client, Http};

/// Per-chain venue wiring built by `app::state::build_state`. One instance per
/// (venue, chain) pair: `quoter_addr` is the venue's on-chain quoter lens and
/// `adapter_addr` the deployed `ISwapAdapter` the emitted route binds to.
pub struct ChainSetup {
    pub provider: RootProvider<Http<Client>>,
    pub quoter_addr: Address,
    pub adapter_addr: Address,
    /// MASP fee bps deducted from the venue's gross output before slippage.
    pub masp_fee_bps: u16,
}

impl ChainSetup {
    /// Turns a venue's raw result into a [`Quote`].
    ///
    /// Every venue shares this tail, and the ordering in it is load-bearing:
    /// the MASP fee is a reciprocal of `gross` (see [`max_deposit`]) and is
    /// applied *before* the caller's slippage, so the fee sits inside the
    /// `min_out` floor rather than stacked outside it. Venue quoters therefore
    /// only produce `gross`, `venue_gas` and `route`, and never assemble a
    /// `Quote` themselves.
    pub fn build_quote(
        &self,
        venue: Venue,
        req: &QuoteRequest,
        gross: U256,
        venue_gas: u64,
        route: Bytes,
    ) -> Quote {
        let expected_out = max_deposit(gross, self.masp_fee_bps);
        Quote {
            venue,
            adapter: self.adapter_addr,
            route,
            min_out: req.apply_slippage(expected_out),
            masp_fee: gross.saturating_sub(expected_out),
            expected_out,
            gas_estimate: venue_gas.saturating_add(WRAPPER_OVERHEAD_GAS),
            quoted_at: now_secs(),
            masp_fee_bps: self.masp_fee_bps,
        }
    }
}
