use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscription {
    pub detection_key_hex: String,
    pub gamma: i32,
    /// Client-chosen capability token, 32 bytes hex. Wallets derive it from
    /// `ivk`, so there is nothing extra to persist and re-registering
    /// re-attaches instead of duplicating.
    ///
    /// It must not be derived from anything a sender or this server already
    /// knows — in particular not from `dk`, which is public in the address
    /// and recoverable from the detection key this request carries.
    pub token_hex: String,
}
